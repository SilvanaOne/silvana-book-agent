// Port of agent-news-sentiment-lexicon. Streams synthetic news headlines,
// scores each on a lexicon in [-1..+1], and emits signals when
// |score| > threshold AND text mentions a configured market alias.

export type Side = "buy" | "sell";

export type Headline = Readonly<{
  seq: number;
  t: number;
  text: string;
  score: number;
  matchedMarkets: string[];
  side: Side | null;
  emitted: boolean;
  skipReason?: "no_market" | "below_threshold";
}>;

export type NewsConfig = Readonly<{
  threshold: number;
  quantity: number;
  aliases: Record<string, string[]>;   // market → aliases (lowercased)
  headlinesPerSec: number;
  headlinePool: string[];              // pool for auto-emit
}>;

export type NewsState = {
  config: NewsConfig;
  status: "running" | "idle";
  headlines: Headline[];               // ~50
  totalSeen: number;
  totalEmitted: number;
  skippedNoMarket: number;
  skippedBelowThr: number;
  byMarket: Record<string, number>;
  bySide: Record<string, number>;
  avgScore: number;
  nextSpawnAt: number;
  nextSeq: number;
};

const MAX_HEADLINES = 50;

export function initState(config: NewsConfig, now: number): NewsState {
  return {
    config, status: "running", headlines: [],
    totalSeen: 0, totalEmitted: 0, skippedNoMarket: 0, skippedBelowThr: 0,
    byMarket: {}, bySide: {}, avgScore: 0,
    nextSpawnAt: now, nextSeq: 1,
  };
}

export function step(state: NewsState, _price: number, now: number): { state: NewsState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  const interval = 1000 / Math.max(0.1, state.config.headlinesPerSec);
  while (now >= state.nextSpawnAt) {
    if (state.config.headlinePool.length === 0) break;
    const text = state.config.headlinePool[Math.floor(Math.random() * state.config.headlinePool.length)];
    processHeadline(state, text, state.nextSpawnAt, log);
    state.nextSpawnAt += interval;
  }
  return { state, log };
}

export function ingestHeadline(state: NewsState, text: string, now: number): { state: NewsState; log: string[] } {
  const log: string[] = [];
  processHeadline(state, text, now, log);
  return { state, log };
}

function processHeadline(state: NewsState, text: string, t: number, log: string[]) {
  const score = sentimentScore(text);
  const matched = matchMarkets(text, state.config.aliases);
  const side: Side | null = score === 0 ? null : score > 0 ? "buy" : "sell";
  const seq = state.nextSeq++;
  const emitted = matched.length > 0 && Math.abs(score) >= state.config.threshold;
  const skipReason = matched.length === 0 ? "no_market" : (Math.abs(score) < state.config.threshold ? "below_threshold" : undefined);
  const h: Headline = { seq, t, text, score, matchedMarkets: matched, side, emitted, skipReason };
  state.headlines.push(h);
  while (state.headlines.length > MAX_HEADLINES) state.headlines.shift();
  state.totalSeen += 1;
  if (emitted) {
    state.totalEmitted += matched.length;
    for (const m of matched) state.byMarket[m] = (state.byMarket[m] ?? 0) + 1;
    if (side) state.bySide[side] = (state.bySide[side] ?? 0) + matched.length;
    const kept = state.headlines.filter((x) => x.emitted);
    state.avgScore = kept.reduce((a, x) => a + Math.abs(x.score), 0) / Math.max(1, kept.length);
    log.push(`EMIT #${seq} score=${score.toFixed(2)} ${side} ${matched.join(",")} · ${truncate(text, 50)}`);
  } else if (skipReason === "no_market") {
    state.skippedNoMarket += 1;
    log.push(`SKIP #${seq} no market match · ${truncate(text, 50)}`);
  } else {
    state.skippedBelowThr += 1;
    log.push(`SKIP #${seq} |${score.toFixed(2)}| < thr · ${truncate(text, 50)}`);
  }
}

function truncate(s: string, n: number) { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }

function matchMarkets(text: string, aliases: Record<string, string[]>): string[] {
  const lower = text.toLowerCase();
  const out: string[] = [];
  for (const [market, aliasList] of Object.entries(aliases)) {
    if (aliasList.some((a) => lower.includes(a))) out.push(market);
  }
  return out;
}

export function sentimentScore(text: string): number {
  const h = text.toLowerCase();
  const pos = ["pump", "rally", "surge", "moon", "bullish", "gain", "up ", "record high", "beats", "positive", "breakthrough", "approved", "adopts"];
  const neg = ["dump", "crash", "plunge", "bearish", "loss", "down", "record low", "misses", "negative", "collapse", "hacked", "banned", "sued", "sec "];
  let score = 0;
  for (const w of pos) if (h.includes(w)) score += 0.25;
  for (const w of neg) if (h.includes(w)) score -= 0.25;
  for (const w of ["massive", "huge", "record"]) if (h.includes(w)) score *= 1.3;
  if (score > 1) score = 1;
  if (score < -1) score = -1;
  return Math.round(score * 100) / 100;
}
