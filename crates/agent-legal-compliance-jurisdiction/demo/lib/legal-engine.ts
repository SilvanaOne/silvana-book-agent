// Port of agent-legal-compliance-jurisdiction. Maps party → jurisdiction, then applies
// per-jurisdiction rules to each simulated settlement:
//   - allowed_markets (allowlist; empty = no restriction)
//   - prohibited_markets (denylist)
//   - max_notional_per_trade (Decimal cap)
//   - prohibited_counterparty_jurisdictions (sanction-style pair block)

export type JurisdictionRules = Readonly<{
  allowedMarkets: string[];
  prohibitedMarkets: string[];
  maxNotionalPerTrade?: number;
  prohibitedCounterpartyJurisdictions: string[];
}>;

export type Policy = Readonly<{
  partyJurisdictions: Record<string, string>;         // party → jurisdiction
  jurisdictions: Record<string, JurisdictionRules>;
}>;

export type LegalConfig = Readonly<{
  policy: Policy;
  eventRatePerSec: number;
  parties: string[];
  markets: string[];
}>;

export type Settlement = Readonly<{
  seq: number;
  t: number;
  buyer: string;
  seller: string;
  buyerJx: string | null;
  sellerJx: string | null;
  market: string;
  notional: number;
}>;

export type Verdict = "legal" | "violation";

export type EvalRecord = Readonly<{
  seq: number;
  t: number;
  settlement: Settlement;
  verdict: Verdict;
  hits: string[];       // e.g. "US.prohibited_market", "EU.max_notional_per_trade"
}>;

export type LegalState = {
  config: LegalConfig;
  status: "running" | "idle";
  nextSeq: number;
  settlementsSeen: number;
  legal: number;
  violation: number;
  hitsByRule: Record<string, number>;
  perJurisdiction: Record<string, { total: number; violations: number }>;
  recentEvals: EvalRecord[];   // ~40
  nextSpawnAt: number;
};

const MAX_RECENT = 40;

export function initState(config: LegalConfig, now: number): LegalState {
  return { config, status: "running", nextSeq: 1, settlementsSeen: 0, legal: 0, violation: 0, hitsByRule: {}, perJurisdiction: {}, recentEvals: [], nextSpawnAt: now };
}

export function step(state: LegalState, _price: number, now: number): { state: LegalState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  const intervalMs = 1000 / Math.max(0.1, state.config.eventRatePerSec);
  while (now >= state.nextSpawnAt) {
    const ev = spawn(state, state.nextSpawnAt);
    state.nextSpawnAt += intervalMs;
    state.settlementsSeen += 1;
    const { verdict, hits } = evaluate(state, ev);
    state.recentEvals.push({ seq: ev.seq, t: ev.t, settlement: ev, verdict, hits });
    while (state.recentEvals.length > MAX_RECENT) state.recentEvals.shift();
    if (verdict === "legal") state.legal += 1;
    else state.violation += 1;
    for (const h of hits) state.hitsByRule[h] = (state.hitsByRule[h] ?? 0) + 1;
    for (const jx of [ev.buyerJx, ev.sellerJx]) {
      if (!jx) continue;
      const b = state.perJurisdiction[jx] ?? { total: 0, violations: 0 };
      b.total += 1;
      if (verdict === "violation") b.violations += 1;
      state.perJurisdiction[jx] = b;
    }
    if (verdict === "violation") log.push(`VIOL  ${ev.buyer}(${ev.buyerJx})→${ev.seller}(${ev.sellerJx}) ${ev.market} ${ev.notional.toFixed(0)} · ${hits.join(",")}`);
  }
  return { state, log };
}

function spawn(state: LegalState, t: number): Settlement {
  const p = state.config.parties;
  let bi = Math.floor(Math.random() * p.length);
  let si = Math.floor(Math.random() * p.length);
  if (si === bi) si = (si + 1) % p.length;
  const market = state.config.markets[Math.floor(Math.random() * state.config.markets.length)];
  const notional = Math.round((500 + Math.random() * 15000) * 100) / 100;
  const jx = state.config.policy.partyJurisdictions;
  return { seq: state.nextSeq++, t, buyer: p[bi], seller: p[si], buyerJx: jx[p[bi]] ?? null, sellerJx: jx[p[si]] ?? null, market, notional };
}

function evaluate(state: LegalState, ev: Settlement): { verdict: Verdict; hits: string[] } {
  const hits: string[] = [];
  const { jurisdictions } = state.config.policy;

  for (const [party, jx] of [[ev.buyer, ev.buyerJx] as const, [ev.seller, ev.sellerJx] as const]) {
    if (!jx) { hits.push("unknown_jurisdiction:" + party); continue; }
    const rules = jurisdictions[jx];
    if (!rules) continue;
    if (rules.allowedMarkets.length > 0 && !rules.allowedMarkets.includes(ev.market)) {
      hits.push(`${jx}.market_not_allowed`);
    }
    if (rules.prohibitedMarkets.includes(ev.market)) hits.push(`${jx}.prohibited_market`);
    if (rules.maxNotionalPerTrade !== undefined && ev.notional > rules.maxNotionalPerTrade) hits.push(`${jx}.max_notional_per_trade`);
  }

  if (ev.buyerJx && ev.sellerJx) {
    const buyerRules = jurisdictions[ev.buyerJx];
    const sellerRules = jurisdictions[ev.sellerJx];
    if (buyerRules?.prohibitedCounterpartyJurisdictions.includes(ev.sellerJx)) hits.push(`${ev.buyerJx}.blocks_${ev.sellerJx}`);
    if (sellerRules?.prohibitedCounterpartyJurisdictions.includes(ev.buyerJx)) hits.push(`${ev.sellerJx}.blocks_${ev.buyerJx}`);
  }

  return { verdict: hits.length === 0 ? "legal" : "violation", hits };
}
