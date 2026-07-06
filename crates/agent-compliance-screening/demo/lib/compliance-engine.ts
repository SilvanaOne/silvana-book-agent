// Port of agent-compliance-screening. Evaluates each simulated settlement
// against 4 rule kinds:
//  - blocked_pairs: (a, b) either direction
//  - party_caps: rolling window daily notional per party
//  - allowed_counterparties: whitelist (empty = no restriction)
//  - blocked_markets: hard-list of markets

export type Policy = Readonly<{
  blockedPairs: Array<[string, string]>;
  partyCaps: Array<{ party: string; windowHours: number; maxNotional: number }>;
  allowedCounterparties: string[];
  blockedMarkets: string[];
}>;

export type ComplianceConfig = Readonly<{
  policy: Policy;
  emitAccepts: boolean;
  market?: string;                  // optional filter
  eventRatePerSec: number;          // simulated settlements per second (avg)
  parties: string[];                // pool of party ids
  markets: string[];                // pool of markets
}>;

export type SettlementEvent = Readonly<{
  seq: number;
  t: number;
  proposalId: string;
  market: string;
  buyer: string;
  seller: string;
  notional: number;
}>;

export type Verdict = "accept" | "reject";

export type EvalRecord = Readonly<{
  seq: number;
  t: number;
  settlement: SettlementEvent;
  verdict: Verdict;
  hits: string[];             // e.g. ["blocked_pair", "party_cap[party::a]"]
}>;

export type ComplianceState = {
  config: ComplianceConfig;
  status: "running" | "idle";
  nextSeq: number;
  eventsSeen: number;
  accepts: number;
  rejects: number;
  hitsByRule: Record<string, number>;
  recentEvals: EvalRecord[];        // ~40
  partyTotals: Record<string, Array<{ t: number; notional: number }>>;
  nextSpawnAt: number;
};

const MAX_RECENT = 40;

export function initState(config: ComplianceConfig, now: number): ComplianceState {
  return {
    config,
    status: "running",
    nextSeq: 1,
    eventsSeen: 0,
    accepts: 0,
    rejects: 0,
    hitsByRule: {},
    recentEvals: [],
    partyTotals: {},
    nextSpawnAt: now,
  };
}

export function step(state: ComplianceState, _price: number, now: number): { state: ComplianceState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];

  const intervalMs = 1000 / Math.max(0.1, state.config.eventRatePerSec);
  while (now >= state.nextSpawnAt) {
    const ev = spawnSettlement(state, state.nextSpawnAt);
    state.nextSpawnAt += intervalMs;
    if (state.config.market && ev.market !== state.config.market) continue;
    const { verdict, hits } = evaluateEvent(state, ev, now);
    state.eventsSeen += 1;
    if (verdict === "accept") state.accepts += 1;
    else state.rejects += 1;
    for (const h of hits) state.hitsByRule[h] = (state.hitsByRule[h] ?? 0) + 1;
    if (verdict === "reject" || state.config.emitAccepts) {
      state.recentEvals.push({ seq: ev.seq, t: ev.t, settlement: ev, verdict, hits });
      while (state.recentEvals.length > MAX_RECENT) state.recentEvals.shift();
      log.push(`${verdict.toUpperCase()} settle=${ev.proposalId} ${ev.market} ${ev.buyer}→${ev.seller} notional=${ev.notional.toFixed(2)}${hits.length ? " · " + hits.join(",") : ""}`);
    }
  }
  return { state, log };
}

function spawnSettlement(state: ComplianceState, t: number): SettlementEvent {
  const { parties, markets } = state.config;
  let buyerIdx = Math.floor(Math.random() * parties.length);
  let sellerIdx = Math.floor(Math.random() * parties.length);
  if (sellerIdx === buyerIdx) sellerIdx = (sellerIdx + 1) % parties.length;
  const market = markets[Math.floor(Math.random() * markets.length)];
  const notional = Math.round((10 + Math.random() * 5000) * 100) / 100;
  const seq = state.nextSeq++;
  return { seq, t, proposalId: `prop_${seq.toString(16)}`, market, buyer: parties[buyerIdx], seller: parties[sellerIdx], notional };
}

function evaluateEvent(state: ComplianceState, ev: SettlementEvent, now: number): { verdict: Verdict; hits: string[] } {
  const hits: string[] = [];
  const p = state.config.policy;

  if (p.blockedMarkets.includes(ev.market)) hits.push("blocked_market");

  for (const [a, b] of p.blockedPairs) {
    if ((ev.buyer === a && ev.seller === b) || (ev.buyer === b && ev.seller === a)) {
      hits.push("blocked_pair");
      break;
    }
  }

  if (p.allowedCounterparties.length > 0) {
    if (!p.allowedCounterparties.includes(ev.buyer) && !p.allowedCounterparties.includes(ev.seller)) {
      hits.push("counterparty_not_allowed");
    }
  }

  for (const cap of p.partyCaps) {
    for (const party of [ev.buyer, ev.seller]) {
      if (party !== cap.party) continue;
      const cutoff = now - cap.windowHours * 3600 * 1000;
      const buf = state.partyTotals[party] ?? [];
      while (buf.length > 0 && buf[0].t < cutoff) buf.shift();
      const total = buf.reduce((a, e) => a + e.notional, 0);
      const projected = total + ev.notional;
      if (projected > cap.maxNotional) {
        hits.push(`party_cap[${party}]`);
      }
      // Only record if we don't reject on cap violation
    }
  }

  const verdict: Verdict = hits.length === 0 ? "accept" : "reject";
  if (verdict === "accept") {
    for (const party of [ev.buyer, ev.seller]) {
      if (!state.partyTotals[party]) state.partyTotals[party] = [];
      state.partyTotals[party].push({ t: now, notional: ev.notional });
    }
  }
  return { verdict, hits };
}
