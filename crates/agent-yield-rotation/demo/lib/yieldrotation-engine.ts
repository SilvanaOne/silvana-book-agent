// Port of agent-yield-rotation ranking logic to TypeScript. Mirrors
// crates/agent-yield-rotation/src/main.rs (rank_markets loop).
//
// Score per market:
//   score = w_change × change_24h_pct
//         + w_volume × log10(volume_24h)
//         − w_spread × spread_pct
//
// Each cycle emits the ranking (sorted desc by score). Whenever the top-ranked
// market changes vs the previous cycle, a rotation_signal event is emitted.

export type MarketMetrics = {
  market: string;
  price: number;
  change24hPct: number;
  volume24h: number;
  spreadPct: number;
  logVolume: number;
  score: number;
};

export type YieldRotationConfig = Readonly<{
  markets: string[];
  wChange: number;
  wVolume: number;
  wSpread: number;
  pollSecs: number;
  startingPrice: number;
}>;

export type RotationEvent = Readonly<{
  t: number;
  from: string | null;
  to: string;
  score: number;
}>;

export type ScoreSnapshot = Readonly<{
  t: number;
  scores: Record<string, number>;
}>;

// Per-market slowly evolving simulator state.
export type MarketSimState = {
  market: string;
  price: number;
  change24hPct: number;      // random-walked in [-5, +15]
  volume24h: number;         // random-walked around a per-market base
  spreadPct: number;         // random-walked in [0.02, 0.15]
  volBase: number;           // remembered mean volume for this market
};

export type YieldRotationState = {
  config: YieldRotationConfig;
  status: "monitoring" | "idle";
  sim: MarketSimState[];
  ranking: MarketMetrics[];  // sorted score desc
  currentTop: string | null;
  prevTop: string | null;
  rotationsCount: number;
  publishedCount: number;
  lastPublishedAt: number | null;
  nextPublishAt: number;     // ms epoch of the next scheduled publish
  rotationEvents: RotationEvent[];
  scoreHistory: ScoreSnapshot[];
};

const MAX_ROTATION_EVENTS = 40;
const MAX_SCORE_HISTORY = 240;

export function initState(config: YieldRotationConfig, now: number): YieldRotationState {
  const sim: MarketSimState[] = config.markets.map((m, i) => {
    // Give each market a mildly different seed so the demo isn't boring.
    const seed = (i + 1) * 0.31 + Math.random() * 0.4;
    const startChange = (Math.random() * 20 - 5) + seed * 2;          // roughly -5..+15
    const volBase = 500_000 * (0.5 + Math.random() * 3);              // ~250k–1.7M
    return {
      market: m,
      price: config.startingPrice * (0.9 + Math.random() * 0.25),
      change24hPct: clamp(startChange, -5, 15),
      volume24h: volBase,
      spreadPct: 0.03 + Math.random() * 0.08,                        // 0.03..0.11
      volBase,
    };
  });

  return {
    config,
    status: "monitoring",
    sim,
    ranking: [],
    currentTop: null,
    prevTop: null,
    rotationsCount: 0,
    publishedCount: 0,
    lastPublishedAt: null,
    nextPublishAt: now,
    rotationEvents: [],
    scoreHistory: [],
  };
}

/** Advance the simulator by one tick and (if scheduled) emit a ranking + rotation. */
export function step(state: YieldRotationState, _price: number, now: number): { state: YieldRotationState; events: string[] } {
  if (state.status !== "monitoring") return { state, events: [] };
  const events: string[] = [];

  // 1) evolve each market's per-tick metrics.
  for (const s of state.sim) {
    // Slow OU-ish drift on change_24h_pct
    s.change24hPct = clamp(s.change24hPct + gauss() * 0.4, -5, 15);
    // Volume: mean-reverting around volBase, positive
    const drift = (s.volBase - s.volume24h) * 0.05;
    s.volume24h = Math.max(1_000, s.volume24h + drift + gauss() * s.volBase * 0.02);
    // Spread: bounded random walk in [0.02, 0.15]
    s.spreadPct = clamp(s.spreadPct + gauss() * 0.005, 0.02, 0.15);
    // Price: gentle GBM around starting price so the "top-market" concept is visible
    s.price = Math.max(0.0001, s.price * (1 + gauss() * 0.004));
  }

  // 2) compute ranking every tick — cheap, needed for the bar chart.
  const ranking = state.sim
    .map<MarketMetrics>((s) => {
      const logVolume = s.volume24h > 0 ? Math.log10(s.volume24h) : 0;
      const score =
        state.config.wChange * s.change24hPct +
        state.config.wVolume * logVolume -
        state.config.wSpread * s.spreadPct;
      return {
        market: s.market,
        price: s.price,
        change24hPct: s.change24hPct,
        volume24h: s.volume24h,
        spreadPct: s.spreadPct,
        logVolume,
        score,
      };
    })
    .sort((a, b) => b.score - a.score);

  state.ranking = ranking;

  // 3) publish + rotation-signal only on the scheduled cadence.
  if (now >= state.nextPublishAt) {
    state.publishedCount += 1;
    state.lastPublishedAt = now;
    state.nextPublishAt = now + state.config.pollSecs * 1000;
    events.push(
      `PUBLISH ranking #${state.publishedCount}: ${ranking
        .map((r) => `${r.market}=${r.score.toFixed(2)}`)
        .join(", ")}`,
    );

    const top = ranking[0]?.market ?? null;
    if (top && top !== state.currentTop) {
      const from = state.currentTop;
      state.prevTop = from;
      state.currentTop = top;
      state.rotationsCount += 1;
      state.rotationEvents.push({ t: now, from, to: top, score: ranking[0].score });
      if (state.rotationEvents.length > MAX_ROTATION_EVENTS) state.rotationEvents.shift();
      events.push(`ROTATION: from ${from ?? "—"} to ${top} (score=${ranking[0].score.toFixed(3)})`);
    }

    // Append a score snapshot for the mini timeline.
    const scores: Record<string, number> = {};
    for (const r of ranking) scores[r.market] = r.score;
    state.scoreHistory.push({ t: now, scores });
    if (state.scoreHistory.length > MAX_SCORE_HISTORY) state.scoreHistory.shift();
  }

  return { state, events };
}

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

function gauss(): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  return Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
}
