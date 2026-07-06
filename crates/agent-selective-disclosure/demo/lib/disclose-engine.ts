// Port of agent-selective-disclosure. Batch tool that reads a signed
// hash-chained trading-history JSONL, applies filters (kinds / markets /
// redact_fields), and emits a fresh hash-chained log signed by this
// party's Ed25519 key. Recipients verify the chain + signatures locally.
//
// This demo:
//  1. Generates a synthetic 60-record history (hash-chained; signature is
//     a fake SHA256 tag since the browser can't hold an Ed25519 key).
//  2. Applies configured filters (same semantics as the Rust binary).
//  3. Emits a filtered log with fresh prev_hash chain + new tags.
//  4. `verify()` walks the emitted chain and checks each prev_hash matches
//     the previous record's fingerprint.

export type Kind =
  | "order.created"
  | "order.filled"
  | "order.cancelled"
  | "settlement.proposal"
  | "settlement.settled"
  | "settlement.failed";

const KINDS: Kind[] = ["order.created", "order.filled", "order.cancelled", "settlement.proposal", "settlement.settled", "settlement.failed"];

export type HistoryRecord = Readonly<{
  seq: number;
  ts: string;                 // ISO 8601
  prev_hash: string;          // hex sha256
  kind: Kind;
  payload: Record<string, unknown>;
  payload_sha256: string;     // hex sha256(payload JSON)
  signing_scheme: "ed25519-sha256-v1";
  signature: string;          // base64 (fake in the demo)
  public_key: string;         // base58 (fake in the demo)
}>;

export type DiscloseConfig = Readonly<{
  historySize: number;                  // records in synthetic input
  kinds: Kind[];                        // empty = keep all
  markets: string[];                    // empty = keep all
  redactFields: string[];               // drop these fields from payload
  parties: string[];
  marketPool: string[];
}>;

export type VerifyReport = Readonly<{
  ok: boolean;
  checked: number;
  brokenAt?: number;
  reason?: string;
}>;

export type DiscloseState = {
  config: DiscloseConfig;
  history: HistoryRecord[];       // input log
  disclosure: HistoryRecord[];    // filtered + re-chained
  filteredCount: number;
  redactedFields: number;
  lastVerify: VerifyReport | null;
  runs: number;
};

export function initState(config: DiscloseConfig): DiscloseState {
  const history = generateHistory(config);
  return { config, history, disclosure: [], filteredCount: 0, redactedFields: 0, lastVerify: null, runs: 0 };
}

export function runFilter(state: DiscloseState): { state: DiscloseState; log: string[] } {
  const log: string[] = [];
  const kindsSet = new Set(state.config.kinds);
  const marketsSet = new Set(state.config.markets);
  const redactSet = new Set(state.config.redactFields);

  let prev = "0".repeat(64);
  const disclosure: HistoryRecord[] = [];
  let redactedCount = 0;

  for (const rec of state.history) {
    if (kindsSet.size > 0 && !kindsSet.has(rec.kind)) continue;
    const marketId = (rec.payload as { market_id?: string }).market_id;
    if (marketsSet.size > 0 && marketId && !marketsSet.has(marketId)) continue;
    const filteredPayload: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(rec.payload)) {
      if (redactSet.has(k)) { redactedCount += 1; continue; }
      filteredPayload[k] = v;
    }
    const seq = disclosure.length + 1;
    const ts = rec.ts;
    const payloadHash = fauxSha256(JSON.stringify(filteredPayload));
    const line = `${seq}|${ts}|${prev}|${rec.kind}|${payloadHash}`;
    const signature = fauxSha256("SIG:" + line);
    const publicKey = "pk_DEMO_11111111111111111111111111111111";
    const newRec: HistoryRecord = {
      seq, ts, prev_hash: prev, kind: rec.kind,
      payload: filteredPayload, payload_sha256: payloadHash,
      signing_scheme: "ed25519-sha256-v1", signature, public_key: publicKey,
    };
    disclosure.push(newRec);
    prev = fauxSha256(line + "|" + signature);
  }

  state.disclosure = disclosure;
  state.filteredCount = disclosure.length;
  state.redactedFields = redactedCount;
  state.runs += 1;
  log.push(`FILTER run #${state.runs}: ${state.history.length} → ${disclosure.length} records · redacted ${redactedCount} fields`);
  return { state, log };
}

export function runVerify(state: DiscloseState): { state: DiscloseState; log: string[] } {
  const log: string[] = [];
  let prev = "0".repeat(64);
  for (let i = 0; i < state.disclosure.length; i++) {
    const rec = state.disclosure[i];
    if (rec.prev_hash !== prev) {
      state.lastVerify = { ok: false, checked: i, brokenAt: rec.seq, reason: "prev_hash mismatch" };
      log.push(`VERIFY FAIL: chain broken at seq=${rec.seq} (expected ${prev.slice(0, 8)}…, saw ${rec.prev_hash.slice(0, 8)}…)`);
      return { state, log };
    }
    const expectedSig = fauxSha256("SIG:" + `${rec.seq}|${rec.ts}|${rec.prev_hash}|${rec.kind}|${rec.payload_sha256}`);
    if (rec.signature !== expectedSig) {
      state.lastVerify = { ok: false, checked: i, brokenAt: rec.seq, reason: "signature mismatch" };
      log.push(`VERIFY FAIL: signature mismatch at seq=${rec.seq}`);
      return { state, log };
    }
    prev = fauxSha256(`${rec.seq}|${rec.ts}|${rec.prev_hash}|${rec.kind}|${rec.payload_sha256}|${rec.signature}`);
  }
  state.lastVerify = { ok: true, checked: state.disclosure.length };
  log.push(`VERIFY OK: chain intact, ${state.disclosure.length} records verified`);
  return { state, log };
}

/** Simulate an editor tampering with record at `seq` — flips a payload field.
 * A subsequent verify() should catch it (prev_hash cascade). */
export function tamperRecord(state: DiscloseState, seq: number): { state: DiscloseState; log: string[] } {
  const idx = state.disclosure.findIndex((r) => r.seq === seq);
  if (idx < 0) return { state, log: [`No record with seq=${seq}`] };
  const rec = state.disclosure[idx];
  const tamperedPayload = { ...rec.payload, TAMPERED: "yes" };
  const tampered = { ...rec, payload: tamperedPayload, payload_sha256: fauxSha256(JSON.stringify(tamperedPayload)) };
  state.disclosure = [...state.disclosure.slice(0, idx), tampered, ...state.disclosure.slice(idx + 1)];
  return { state, log: [`TAMPER: seq=${seq} payload edited — next verify should fail`] };
}

function generateHistory(config: DiscloseConfig): HistoryRecord[] {
  const now = Date.now();
  const out: HistoryRecord[] = [];
  let prev = "0".repeat(64);
  for (let i = 0; i < config.historySize; i++) {
    const kind = KINDS[Math.floor(Math.random() * KINDS.length)];
    const market = config.marketPool[Math.floor(Math.random() * config.marketPool.length)];
    const buyer = config.parties[Math.floor(Math.random() * config.parties.length)];
    let seller = config.parties[Math.floor(Math.random() * config.parties.length)];
    if (seller === buyer) seller = config.parties[(config.parties.indexOf(buyer) + 1) % config.parties.length];
    const payload: Record<string, unknown> = { market_id: market, buyer, seller, notional: Math.round((100 + Math.random() * 5000) * 100) / 100 };
    if (kind.startsWith("order.")) { payload.order_id = `ord_${i}`; payload.side = Math.random() < 0.5 ? "BID" : "OFFER"; }
    else { payload.proposal_id = `prop_${i}`; }
    const ts = new Date(now - (config.historySize - i) * 5000).toISOString();
    const seq = i + 1;
    const payloadHash = fauxSha256(JSON.stringify(payload));
    const line = `${seq}|${ts}|${prev}|${kind}|${payloadHash}`;
    const signature = fauxSha256("SIG:" + line);
    const rec: HistoryRecord = {
      seq, ts, prev_hash: prev, kind, payload,
      payload_sha256: payloadHash, signing_scheme: "ed25519-sha256-v1",
      signature, public_key: "pk_DEMO_11111111111111111111111111111111",
    };
    out.push(rec);
    prev = fauxSha256(line + "|" + signature);
  }
  return out;
}

// Fast web-safe fake sha256 (not cryptographic — deterministic demo hash).
function fauxSha256(input: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) h = (h ^ input.charCodeAt(i)) * 0x01000193 >>> 0;
  // Expand to 64 hex chars by mixing 8 rotations.
  let out = "";
  for (let r = 0; r < 8; r++) {
    h = (h * 2654435761 + r) >>> 0;
    out += ("00000000" + h.toString(16)).slice(-8);
  }
  return out;
}
