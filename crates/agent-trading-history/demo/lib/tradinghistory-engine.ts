// Port of agent-trading-history logic to TypeScript. Mirrors
// crates/agent-trading-history/src/main.rs (fn append + fn verify).
//
// Records:
//   subscribe to order + settlement streams
//   each event → append JSONL line as { seq, ts, prev_hash, kind, payload,
//                                       payload_sha256, signature, public_key }
//   prev_hash chains records into an append-only tamper-evident log
//   editing any line invalidates the chain from that point on
//
// This demo simulates the event stream with a Poisson process and uses a
// simple non-cryptographic hash (djb2-like) — the point is to visualize
// the *chain*, not to actually secure anything.

export type EventKind =
  | "order.created"
  | "order.filled"
  | "order.cancelled"
  | "settlement.settled"
  | "settlement.failed";

export type HistoryRecord = {
  seq: number;
  t: number;                 // epoch ms
  kind: EventKind;
  payload: Record<string, unknown>;
  prevHash: string;
  hash: string;
  signature: string;
  verified: boolean;         // chain check: prev_hash matches previous.hash + own hash matches recompute
  tampered?: boolean;        // set when payload was mutated after signing
};

export type TradingHistoryConfig = Readonly<{
  orders: boolean;              // include order.* events?
  settlements: boolean;         // include settlement.* events?
  market?: string;              // optional market filter
  eventArrivalPerTick: number;  // Poisson lambda per tick, e.g. 0.3
  startingPrice: number;
}>;

export type TradingHistoryState = {
  config: TradingHistoryConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  records: HistoryRecord[];         // bounded ring — see MAX_RECORDS
  recordsCount: number;             // lifetime records appended
  chainOK: boolean;
  tamperedIndex: number | null;     // index (within records[]) of first tampered/broken link
  verifiedCount: number;
  lastVerifiedAt?: number;
  lastKind?: EventKind;
  lastHash?: string;
  lastSeq?: number;
  rateWindow: number[];             // timestamps of recent records (for rate)
};

const MAX_RECORDS = 30;
const RATE_WINDOW_MS = 60_000;

export const GENESIS = "GENESIS";

/** Non-cryptographic hash suitable for a *visual* chain demo. djb2 variant. */
function simpleHash(input: string): string {
  let h = 5381;
  for (let i = 0; i < input.length; i++) {
    h = ((h << 5) - h + input.charCodeAt(i)) | 0;
  }
  // Fold with a second pass so short strings still spread across 16 hex chars.
  let g = 2166136261;
  for (let i = 0; i < input.length; i++) {
    g = Math.imul(g ^ input.charCodeAt(i), 16777619);
  }
  const hex1 = (h >>> 0).toString(16).padStart(8, "0");
  const hex2 = (g >>> 0).toString(16).padStart(8, "0");
  return hex1 + hex2;
}

/** Base64ish mock — we're not really signing here, we just want visual variety. */
function mockSignature(canonical: string): string {
  const a = simpleHash(canonical + "|sig-a");
  const b = simpleHash(canonical + "|sig-b");
  const c = simpleHash(canonical + "|sig-c");
  const d = simpleHash(canonical + "|sig-d");
  return (a + b + c + d).slice(0, 44) + "=";
}

function canonicalOf(seq: number, t: number, prevHash: string, kind: EventKind, payload: Record<string, unknown>): string {
  const payloadStr = JSON.stringify(payload);
  const payloadHash = simpleHash(payloadStr);
  return `seq=${seq}\nts=${new Date(t).toISOString()}\nprev_hash=${prevHash}\nkind=${kind}\npayload_sha256=${payloadHash}\n`;
}

export function initState(config: TradingHistoryConfig): TradingHistoryState {
  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    records: [],
    recordsCount: 0,
    chainOK: true,
    tamperedIndex: null,
    verifiedCount: 0,
    rateWindow: [],
  };
}

function pickKind(rng: () => number, config: TradingHistoryConfig): EventKind | null {
  const pool: EventKind[] = [];
  if (config.orders) {
    pool.push("order.created", "order.filled", "order.cancelled");
  }
  if (config.settlements) {
    pool.push("settlement.settled", "settlement.failed");
  }
  if (pool.length === 0) return null;
  return pool[Math.floor(rng() * pool.length)];
}

function poissonSample(lambda: number, rng: () => number): number {
  if (lambda <= 0) return 0;
  // Knuth's algorithm — fine for small lambda.
  const L = Math.exp(-lambda);
  let k = 0;
  let p = 1;
  do {
    k += 1;
    p *= rng();
  } while (p > L);
  return k - 1;
}

function makePayload(kind: EventKind, price: number, market: string, rng: () => number): Record<string, unknown> {
  const id = Math.floor(rng() * 1_000_000).toString(16).padStart(6, "0");
  const qty = Math.round(rng() * 100) / 10;
  const side = rng() > 0.5 ? "BID" : "OFFER";
  if (kind.startsWith("order.")) {
    return {
      order_id: `ord-${id}`,
      market_id: market,
      side,
      status: kind === "order.created" ? "ACTIVE" : kind === "order.filled" ? "FILLED" : "CANCELLED",
      price: Number(price.toFixed(6)),
      quantity: qty,
    };
  }
  // settlement.*
  return {
    proposal_id: `prop-${id}`,
    market_id: market,
    buyer: `party-${Math.floor(rng() * 100)}`,
    seller: `party-${Math.floor(rng() * 100)}`,
    base_quantity: qty,
    settlement_price: Number(price.toFixed(6)),
    status: kind === "settlement.settled" ? "SETTLED" : "FAILED",
  };
}

/** Applies one tick: samples arrival(s), appends records, updates verified metrics. */
export function step(
  state: TradingHistoryState,
  price: number,
  now: number,
  rng: () => number = Math.random,
): { state: TradingHistoryState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  const arrivals = poissonSample(state.config.eventArrivalPerTick, rng);
  const market = state.config.market ?? "CC-USDC";

  for (let i = 0; i < arrivals; i++) {
    const kind = pickKind(rng, state.config);
    if (!kind) break;

    const seq = state.recordsCount + 1;
    const t = now + i;
    const prevHash = state.records.length === 0 ? GENESIS : state.records[state.records.length - 1].hash;
    const payload = makePayload(kind, price, market, rng);
    const canonical = canonicalOf(seq, t, prevHash, kind, payload);
    const hash = simpleHash(canonical);
    const signature = mockSignature(canonical);

    const rec: HistoryRecord = {
      seq,
      t,
      kind,
      payload,
      prevHash,
      hash,
      signature,
      verified: true,
    };
    state.records.push(rec);
    if (state.records.length > MAX_RECORDS) {
      // Drop the oldest but never break chain — the ring buffer keeps hash
      // linkage between adjacent surviving records (prevHash of the new head
      // still equals the ejected tail's hash we just dropped, but subsequent
      // records still chain correctly among themselves).
      state.records.shift();
      if (state.tamperedIndex !== null) state.tamperedIndex = Math.max(0, state.tamperedIndex - 1);
    }
    state.recordsCount += 1;
    state.lastKind = kind;
    state.lastHash = hash;
    state.lastSeq = seq;
    state.rateWindow.push(t);
    events.push(`RECORD #${seq} ${kind} — hash=${hash.slice(0, 8)} (chain OK)`);
  }

  // Trim rate window.
  const cutoff = now - RATE_WINDOW_MS;
  while (state.rateWindow.length && state.rateWindow[0] < cutoff) state.rateWindow.shift();

  // Re-verify chain end-to-end (cheap: ≤30 records).
  const check = verifyChain(state.records);
  state.chainOK = check.ok;
  state.tamperedIndex = check.tamperedIndex;
  state.verifiedCount = check.verifiedCount;
  state.lastVerifiedAt = now;

  return { state, events };
}

/** Verifies chain + recomputes verified flag on each record. Returns first bad index. */
export function verifyChain(records: HistoryRecord[]): { ok: boolean; tamperedIndex: number | null; verifiedCount: number } {
  let ok = true;
  let tamperedIndex: number | null = null;
  let verifiedCount = 0;
  for (let i = 0; i < records.length; i++) {
    const rec = records[i];
    const expectedPrev = i === 0 ? (rec.prevHash === GENESIS ? GENESIS : records[0].prevHash) : records[i - 1].hash;
    const canonical = canonicalOf(rec.seq, rec.t, rec.prevHash, rec.kind, rec.payload);
    const recomputedHash = simpleHash(canonical);
    // Tampered payload → recomputedHash won't equal stored hash.
    const payloadOK = recomputedHash === rec.hash;
    // Chain link → prev_hash matches previous record's hash (or GENESIS for the first).
    const linkOK = rec.prevHash === expectedPrev;
    const localOK = payloadOK && linkOK && !rec.tampered;
    rec.verified = localOK && ok;
    if (!localOK) {
      if (ok) {
        ok = false;
        tamperedIndex = i;
      }
    }
    if (rec.verified) verifiedCount += 1;
  }
  return { ok, tamperedIndex, verifiedCount };
}

/** Mutates one record's payload without recomputing its hash — breaks chain. */
export function tamperRecord(state: TradingHistoryState, index: number): boolean {
  if (index < 0 || index >= state.records.length) return false;
  const rec = state.records[index];
  // Poison the payload but leave hash+signature untouched.
  rec.payload = { ...rec.payload, _tampered_by_demo: true, mutated_at: Date.now() };
  rec.tampered = true;
  const check = verifyChain(state.records);
  state.chainOK = check.ok;
  state.tamperedIndex = check.tamperedIndex;
  state.verifiedCount = check.verifiedCount;
  state.lastVerifiedAt = Date.now();
  return true;
}
