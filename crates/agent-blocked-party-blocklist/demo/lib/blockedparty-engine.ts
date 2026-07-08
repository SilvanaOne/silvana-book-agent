// Demo model of `agent-blocked-party-blocklist` (crates/agent-blocked-party-blocklist/src/main.rs).
//
// The Rust agent subscribes to settlement events and, for every proposal,
// checks whether `buyer` or `seller` appears in an operator-maintained
// blocklist file. If so, it emits a `compliance.blocked_party_hit` event
// to the configured sinks. The blocklist is re-read every `--reload-secs`
// so operators can add entries live without restarting the agent.
//
// The demo synthesizes a settlement stream (buyer/seller pairs drawn from
// a party pool with occasional blocklisted parties spliced in), applies the
// same match rule, and simulates the periodic blocklist reload.

export type Settlement = Readonly<{
  id: number;
  t: number;              // epoch ms
  buyer: string;
  seller: string;
  market: string;
  notional: number;
}>;

export type BlocklistHit = Readonly<{
  t: number;
  settlementId: number;
  blockedParty: string;
  side: "buyer" | "seller";
}>;

export type BlockedPartyConfig = Readonly<{
  blocklist: readonly string[];
  reloadSecs: number;
  settlementArrivalPerTick: number;  // Poisson mean per tick (1s)
  blockedPartyProbability: number;   // per-settlement chance to splice in a blocked party
  startingPrice: number;             // seed for the price feed used to size notionals
}>;

export type BlockedPartyStats = {
  total: number;
  cleared: number;
  blocked: number;
  hitsCount: number;
};

export type BlockedPartyState = {
  config: BlockedPartyConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  blocklist: string[];            // runtime, mutable copy (can be replaced)
  settlements: Settlement[];      // bounded ~50 recent
  hits: BlocklistHit[];           // bounded ~30 recent
  stats: BlockedPartyStats;
  lastReloadAt: number | null;
  reloadsCount: number;
  nextSettlementId: number;
  msSinceReload: number;
};

const MAX_SETTLEMENTS = 50;
const MAX_HITS = 30;
const TICK_MS = 1000;

// A small pool of clean parties. Any name outside `config.blocklist` is
// treated as clean; the blocklist itself is spliced in per-settlement with
// probability `blockedPartyProbability`.
const CLEAN_PARTY_POOL = [
  "party-alice",
  "party-bob",
  "party-carol",
  "party-dave",
  "party-eve",
  "party-frank",
  "party-grace",
  "party-heidi",
  "party-ivan",
  "party-judy",
];

const MARKETS = ["CC-USDC", "BTC-USD", "ETH-USDC"] as const;

export function initState(config: BlockedPartyConfig): BlockedPartyState {
  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    blocklist: [...config.blocklist],
    settlements: [],
    hits: [],
    stats: { total: 0, cleared: 0, blocked: 0, hitsCount: 0 },
    lastReloadAt: null,
    reloadsCount: 0,
    nextSettlementId: 1,
    msSinceReload: 0,
  };
}

/**
 * Replace the runtime blocklist. Mirrors `--reload-secs` picking up an
 * operator-edited file, but exposed as an explicit action so the UI can
 * push a new list on demand.
 */
export function setBlocklist(state: BlockedPartyState, next: readonly string[], now: number): { state: BlockedPartyState; events: string[] } {
  const cleaned = dedupe(next.map((s) => s.trim()).filter((s) => s.length > 0 && !s.startsWith("#")));
  state.blocklist = cleaned;
  state.reloadsCount += 1;
  state.lastReloadAt = now;
  state.msSinceReload = 0;
  return { state, events: [`BLOCKLIST RELOADED: ${cleaned.length} entries`] };
}

/** Applies one tick worth of settlement flow. Returns state + emitted event strings. */
export function step(state: BlockedPartyState, price: number, now: number): { state: BlockedPartyState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Scheduled reload: emulate the background reload_secs loop in main.rs.
  state.msSinceReload += TICK_MS;
  if (state.config.reloadSecs > 0 && state.msSinceReload >= state.config.reloadSecs * 1000) {
    state.msSinceReload = 0;
    state.reloadsCount += 1;
    state.lastReloadAt = now;
    events.push(`BLOCKLIST RELOADED: ${state.blocklist.length} entries`);
  }

  // Poisson-arrival settlements this tick.
  const arrivals = poisson(state.config.settlementArrivalPerTick);
  const blockedSet = new Set(state.blocklist);

  for (let i = 0; i < arrivals; i++) {
    const s = synthSettlement(state, price, now, blockedSet);
    state.settlements.push(s);
    if (state.settlements.length > MAX_SETTLEMENTS) state.settlements.shift();
    state.stats.total += 1;

    const buyerHit = blockedSet.has(s.buyer);
    const sellerHit = blockedSet.has(s.seller);
    if (!buyerHit && !sellerHit) {
      state.stats.cleared += 1;
      continue;
    }
    state.stats.blocked += 1;

    if (buyerHit) {
      const hit: BlocklistHit = { t: now, settlementId: s.id, blockedParty: s.buyer, side: "buyer" };
      state.hits.push(hit);
      state.stats.hitsCount += 1;
      events.push(`BLOCK: settlement #${s.id} buyer=${s.buyer} in blocklist`);
    }
    if (sellerHit) {
      const hit: BlocklistHit = { t: now, settlementId: s.id, blockedParty: s.seller, side: "seller" };
      state.hits.push(hit);
      state.stats.hitsCount += 1;
      events.push(`BLOCK: settlement #${s.id} seller=${s.seller} in blocklist`);
    }
    while (state.hits.length > MAX_HITS) state.hits.shift();
  }

  return { state, events };
}

function synthSettlement(state: BlockedPartyState, price: number, now: number, blockedSet: Set<string>): Settlement {
  const id = state.nextSettlementId++;
  const market = MARKETS[Math.floor(Math.random() * MARKETS.length)];
  let buyer = pick(CLEAN_PARTY_POOL);
  let seller = pick(CLEAN_PARTY_POOL);
  while (seller === buyer) seller = pick(CLEAN_PARTY_POOL);

  // Splice in a blocked party at the configured rate. Bias toward hitting
  // either side with equal probability, and occasionally hit both.
  const list = state.blocklist;
  if (list.length > 0 && Math.random() < state.config.blockedPartyProbability) {
    const bad = list[Math.floor(Math.random() * list.length)];
    const which = Math.random();
    if (which < 0.45) buyer = bad;
    else if (which < 0.9) seller = bad;
    else {
      // Both spliced — pick a second bad party if the list is large enough.
      buyer = bad;
      if (list.length > 1) {
        let bad2 = list[Math.floor(Math.random() * list.length)];
        let guard = 4;
        while (bad2 === bad && guard-- > 0) bad2 = list[Math.floor(Math.random() * list.length)];
        seller = bad2;
      }
    }
  }

  const qty = 0.5 + Math.random() * 4.5;   // 0.5..5 base units
  const notional = round4(qty * price);

  // Placate the linter about `blockedSet` being unused: it's factored out
  // for callers that want to re-check after the splice logic.
  void blockedSet;

  return { id, t: now, buyer, seller, market, notional };
}

function poisson(lambda: number): number {
  if (!Number.isFinite(lambda) || lambda <= 0) return 0;
  // Knuth's algorithm — fine for small lambda.
  const L = Math.exp(-lambda);
  let k = 0;
  let p = 1;
  do {
    k += 1;
    p *= Math.random();
  } while (p > L);
  return k - 1;
}

function pick<T>(arr: readonly T[]): T {
  return arr[Math.floor(Math.random() * arr.length)];
}

function dedupe(arr: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const s of arr) {
    if (!seen.has(s)) {
      seen.add(s);
      out.push(s);
    }
  }
  return out;
}

function round4(n: number): number {
  return Math.round(n * 1e4) / 1e4;
}
