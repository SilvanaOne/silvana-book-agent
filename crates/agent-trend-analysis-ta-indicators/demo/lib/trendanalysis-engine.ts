// Port of agent-trend-analysis-ta-indicators to TypeScript. Mirrors
// crates/agent-trend-analysis-ta-indicators/src/main.rs — a read-only technical-analysis
// publisher that polls mid, maintains a rolling buffer, and computes:
//   * SMA over `window` samples
//   * EMA with alpha = 2 / (window + 1)
//   * Bollinger Bands = SMA ± bollingerK × stddev(window)
//   * RSI (Wilder) over `rsiPeriod`
//   * MACD = EMA(macdFast) − EMA(macdSlow) with signal = EMA(macd, macdSignal)
// No orders, no ledger writes — pure indicator publisher.

export type TrendAnalysisConfig = Readonly<{
  market: string;
  window: number;           // SMA / EMA / Bollinger window
  rsiPeriod: number;        // typical 14
  bollingerK: number;       // typical 2.0
  macdFast: number;         // typical 12
  macdSlow: number;         // typical 26
  macdSignal: number;       // typical 9
  startingPrice: number;    // simulator seed
}>;

export type TrendAnalysisState = {
  config: TrendAnalysisConfig;
  status: "monitoring" | "idle";
  alphaEma: number;
  alphaFast: number;
  alphaSlow: number;
  alphaSignal: number;
  currentPrice: number;
  priceHistory: number[];   // bounded to max(window, macdSlow) + macdSignal
  // Indicators — null until enough samples are seen.
  sma: number | null;
  ema: number | null;
  bollingerUpper: number | null;
  bollingerMiddle: number | null;
  bollingerLower: number | null;
  bollingerStd: number | null;
  bandPositionPct: number | null;    // 0 = at lower, 100 = at upper
  rsi: number | null;
  rsiAvgGain: number | null;
  rsiAvgLoss: number | null;
  emaFast: number | null;
  emaSlow: number | null;
  macdLine: number | null;
  macdSignalLine: number | null;
  macdHistogram: number | null;
  publishedCount: number;
  lastPublishedAt?: number;
};

export function initState(config: TrendAnalysisConfig, startPrice: number): TrendAnalysisState {
  const historyCap = Math.max(config.window, config.macdSlow) + config.macdSignal + 4;
  return {
    config,
    status: "monitoring",
    alphaEma: 2 / (config.window + 1),
    alphaFast: 2 / (config.macdFast + 1),
    alphaSlow: 2 / (config.macdSlow + 1),
    alphaSignal: 2 / (config.macdSignal + 1),
    currentPrice: startPrice,
    priceHistory: [],
    sma: null,
    ema: null,
    bollingerUpper: null,
    bollingerMiddle: null,
    bollingerLower: null,
    bollingerStd: null,
    bandPositionPct: null,
    rsi: null,
    rsiAvgGain: null,
    rsiAvgLoss: null,
    emaFast: null,
    emaSlow: null,
    macdLine: null,
    macdSignalLine: null,
    macdHistogram: null,
    publishedCount: 0,
  };
}

/** Applies a tick — pushes price, updates all indicators, emits one INDICATORS line. */
export function step(state: TrendAnalysisState, price: number, now: number): { state: TrendAnalysisState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  const c = state.config;
  state.currentPrice = price;

  // Push into bounded history.
  const historyCap = Math.max(c.window, c.macdSlow) + c.macdSignal + 4;
  const prevPrice = state.priceHistory.length > 0 ? state.priceHistory[state.priceHistory.length - 1] : null;
  state.priceHistory.push(price);
  while (state.priceHistory.length > historyCap) state.priceHistory.shift();

  const hist = state.priceHistory;
  const n = hist.length;

  // ---- SMA / Bollinger over last `window` samples ----
  if (n >= c.window) {
    const slice = hist.slice(-c.window);
    const sum = slice.reduce((a, b) => a + b, 0);
    const mean = sum / c.window;
    const variance = slice.reduce((a, b) => a + (b - mean) * (b - mean), 0) / c.window;
    const std = Math.sqrt(variance);
    state.sma = mean;
    state.bollingerMiddle = mean;
    state.bollingerStd = std;
    state.bollingerUpper = mean + c.bollingerK * std;
    state.bollingerLower = mean - c.bollingerK * std;
    if (state.bollingerUpper !== state.bollingerLower) {
      state.bandPositionPct = ((price - state.bollingerLower) / (state.bollingerUpper - state.bollingerLower)) * 100;
    } else {
      state.bandPositionPct = 50;
    }
  } else {
    // Provisional SMA on whatever samples we have.
    const sum = hist.reduce((a, b) => a + b, 0);
    state.sma = sum / n;
  }

  // ---- EMA (window) ----
  state.ema = state.ema === null ? price : state.alphaEma * price + (1 - state.alphaEma) * state.ema;

  // ---- MACD ----
  state.emaFast = state.emaFast === null ? price : state.alphaFast * price + (1 - state.alphaFast) * state.emaFast;
  state.emaSlow = state.emaSlow === null ? price : state.alphaSlow * price + (1 - state.alphaSlow) * state.emaSlow;
  const macdLine = state.emaFast - state.emaSlow;
  state.macdLine = macdLine;
  state.macdSignalLine = state.macdSignalLine === null ? macdLine : state.alphaSignal * macdLine + (1 - state.alphaSignal) * state.macdSignalLine;
  state.macdHistogram = macdLine - state.macdSignalLine;

  // ---- RSI (Wilder smoothing) ----
  if (prevPrice !== null) {
    const diff = price - prevPrice;
    const gain = diff > 0 ? diff : 0;
    const loss = diff < 0 ? -diff : 0;
    if (state.rsiAvgGain === null || state.rsiAvgLoss === null) {
      // Seed RSI once we have `rsiPeriod + 1` samples (rsiPeriod diffs).
      if (n >= c.rsiPeriod + 1) {
        let sumGain = 0;
        let sumLoss = 0;
        for (let i = n - c.rsiPeriod; i < n; i++) {
          const d = hist[i] - hist[i - 1];
          if (d > 0) sumGain += d;
          else sumLoss -= d;
        }
        state.rsiAvgGain = sumGain / c.rsiPeriod;
        state.rsiAvgLoss = sumLoss / c.rsiPeriod;
      }
    } else {
      // Wilder step.
      state.rsiAvgGain = (state.rsiAvgGain * (c.rsiPeriod - 1) + gain) / c.rsiPeriod;
      state.rsiAvgLoss = (state.rsiAvgLoss * (c.rsiPeriod - 1) + loss) / c.rsiPeriod;
    }
    if (state.rsiAvgGain !== null && state.rsiAvgLoss !== null) {
      if (state.rsiAvgLoss === 0) {
        state.rsi = 100;
      } else {
        const rs = state.rsiAvgGain / state.rsiAvgLoss;
        state.rsi = 100 - 100 / (1 + rs);
      }
    }
  }

  // ---- Publish ----
  state.publishedCount += 1;
  state.lastPublishedAt = now;
  const parts: string[] = [
    `INDICATORS #${state.publishedCount}`,
    `mid=${fmt(price)}`,
    state.sma !== null ? `SMA=${fmt(state.sma)}` : `SMA=—`,
    state.ema !== null ? `EMA=${fmt(state.ema)}` : `EMA=—`,
    state.rsi !== null ? `RSI=${state.rsi.toFixed(1)}` : `RSI=—`,
    state.macdLine !== null ? `MACD=${sgn(macdLine)}${fmt(Math.abs(macdLine))}` : `MACD=—`,
    state.bollingerLower !== null && state.bollingerUpper !== null
      ? `BB=(${fmt(state.bollingerLower)}..${fmt(state.bollingerUpper)})`
      : `BB=—`,
  ];
  events.push(parts.join(" "));
  return { state, events };
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

function sgn(n: number): string {
  return n >= 0 ? "+" : "−";
}
