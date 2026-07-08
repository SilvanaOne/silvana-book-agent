// Port of agent-scam-screening-threat-feed. Maintains a set of category buckets, each
// with a severity and a list of party ids. Every simulated settlement is
// screened; matched categories tag the alert. Categories periodically
// "refresh" — in a real deployment this is a file or URL reload.

export type Severity = "info" | "warn" | "critical";

export type Category = Readonly<{
  name: string;
  severity: Severity;
  parties: string[];         // party ids
}>;

export type ScamConfig = Readonly<{
  categories: Category[];
  eventRatePerSec: number;
  refreshSecs: number;       // ticks the demo occasionally injects new parties into categories
  parties: string[];         // pool used to generate settlements
  markets: string[];
}>;

export type Settlement = Readonly<{
  seq: number;
  t: number;
  buyer: string;
  seller: string;
  market: string;
  notional: number;
}>;

export type Match = Readonly<{ category: string; severity: Severity; hitParty: string }>;

export type EvalRecord = Readonly<{
  seq: number;
  t: number;
  settlement: Settlement;
  clean: boolean;
  matches: Match[];
}>;

export type ScamState = {
  config: ScamConfig;
  status: "running" | "idle";
  nextSeq: number;
  settlementsSeen: number;
  clean: number;
  info: number;
  warn: number;
  critical: number;
  perCategoryHits: Record<string, number>;
  recentEvals: EvalRecord[];      // ~40
  nextSpawnAt: number;
  nextRefreshAt: number;
  refreshes: number;
};

const MAX_RECENT = 40;

export function initState(config: ScamConfig, now: number): ScamState {
  return { config, status: "running", nextSeq: 1, settlementsSeen: 0, clean: 0, info: 0, warn: 0, critical: 0, perCategoryHits: {}, recentEvals: [], nextSpawnAt: now, nextRefreshAt: now + config.refreshSecs * 1000, refreshes: 0 };
}

export function step(state: ScamState, _price: number, now: number): { state: ScamState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  const interval = 1000 / Math.max(0.1, state.config.eventRatePerSec);
  while (now >= state.nextSpawnAt) {
    const ev = spawn(state, state.nextSpawnAt);
    state.nextSpawnAt += interval;
    state.settlementsSeen += 1;
    const matches = evaluate(state, ev);
    state.recentEvals.push({ seq: ev.seq, t: ev.t, settlement: ev, clean: matches.length === 0, matches });
    while (state.recentEvals.length > MAX_RECENT) state.recentEvals.shift();

    if (matches.length === 0) state.clean += 1;
    else {
      // Top severity dominates the counter.
      const top = matches.reduce<Severity>((acc, m) => sevLevel(m.severity) > sevLevel(acc) ? m.severity : acc, "info");
      if (top === "critical") state.critical += 1;
      else if (top === "warn") state.warn += 1;
      else state.info += 1;
      for (const m of matches) state.perCategoryHits[m.category] = (state.perCategoryHits[m.category] ?? 0) + 1;
      log.push(`${top.toUpperCase()}  ${ev.buyer}→${ev.seller} on ${ev.market} · ${matches.map((m) => m.category + "[" + truncate(m.hitParty, 12) + "]").join(",")}`);
    }
  }
  if (now >= state.nextRefreshAt) {
    state.refreshes += 1;
    // Simulate a threat-feed refresh: 20% chance we add a random pool-party to some warn/info category.
    if (Math.random() < 0.6) {
      const cat = state.config.categories.find((c) => c.severity !== "critical");
      if (cat) {
        const cand = state.config.parties[Math.floor(Math.random() * state.config.parties.length)];
        if (!cat.parties.includes(cand)) {
          (cat.parties as string[]).push(cand);
          log.push(`REFRESH  ${cat.name}: added ${cand}`);
        }
      }
    }
    state.nextRefreshAt = now + state.config.refreshSecs * 1000;
  }
  return { state, log };
}

function spawn(state: ScamState, t: number): Settlement {
  const p = state.config.parties;
  let bi = Math.floor(Math.random() * p.length);
  let si = Math.floor(Math.random() * p.length);
  if (si === bi) si = (si + 1) % p.length;
  const market = state.config.markets[Math.floor(Math.random() * state.config.markets.length)];
  return { seq: state.nextSeq++, t, buyer: p[bi], seller: p[si], market, notional: Math.round((100 + Math.random() * 8000) * 100) / 100 };
}

function evaluate(state: ScamState, ev: Settlement): Match[] {
  const hits: Match[] = [];
  for (const c of state.config.categories) {
    for (const party of c.parties) {
      if (party === ev.buyer) { hits.push({ category: c.name, severity: c.severity, hitParty: ev.buyer }); break; }
      if (party === ev.seller) { hits.push({ category: c.name, severity: c.severity, hitParty: ev.seller }); break; }
    }
  }
  return hits;
}

function sevLevel(s: Severity): number { return s === "critical" ? 3 : s === "warn" ? 2 : 1; }
function truncate(s: string, n: number): string { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }
