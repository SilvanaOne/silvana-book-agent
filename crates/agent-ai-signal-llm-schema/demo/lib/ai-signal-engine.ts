// Port of agent-ai-signal-llm-schema. Mocks an LLM entirely deterministically for the
// demo — same rules as crates/agent-ai-signal-llm-schema/src/main.rs::mock_llm.

export type Side = "BID" | "OFFER";

export type Signal = Readonly<{
  seq: number;
  t: number;
  prompt: string;
  market: string;
  side: Side;
  price: number;
  quantity: number;
  confidence: number;
  rationale: string;
  dropped: boolean;              // true when confidence < min
}>;

export type AiSignalConfig = Readonly<{
  markets: string[];
  mids: number[];
  minConfidence: number;
  model: string;
  autoPromptEverySec: number;    // 0 = manual only
  prompts: string[];             // pool for auto-emit
}>;

export type AiSignalState = {
  config: AiSignalConfig;
  status: "running" | "idle";
  signals: Signal[];
  totalEmitted: number;
  totalDropped: number;
  byMarket: Record<string, number>;
  bySide: Record<string, number>;
  avgConfidence: number;
  nextEmitAt: number | null;
  nextSeq: number;
};

const MAX_SIGNALS = 60;

export function initState(config: AiSignalConfig, now: number): AiSignalState {
  return {
    config,
    status: "running",
    signals: [],
    totalEmitted: 0,
    totalDropped: 0,
    byMarket: {},
    bySide: {},
    avgConfidence: 0,
    nextEmitAt: config.autoPromptEverySec > 0 ? now + config.autoPromptEverySec * 1000 : null,
    nextSeq: 1,
  };
}

export function step(state: AiSignalState, _price: number, now: number): { state: AiSignalState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  if (state.nextEmitAt !== null && now >= state.nextEmitAt) {
    const prompts = state.config.prompts;
    if (prompts.length > 0) {
      const p = prompts[Math.floor(Math.random() * prompts.length)];
      const s = emitInternal(state, p, now);
      log.push(logLine(s));
    }
    state.nextEmitAt = now + state.config.autoPromptEverySec * 1000;
  }
  return { state, log };
}

export function emitPrompt(state: AiSignalState, prompt: string, now: number): { state: AiSignalState; log: string[] } {
  const s = emitInternal(state, prompt, now);
  return { state, log: [logLine(s)] };
}

function emitInternal(state: AiSignalState, prompt: string, now: number): Signal {
  const out = mockLlm(prompt, state.config.markets, state.config.mids);
  const dropped = out.confidence < state.config.minConfidence;
  const s: Signal = { seq: state.nextSeq++, t: now, prompt, ...out, dropped };
  state.signals.push(s);
  while (state.signals.length > MAX_SIGNALS) state.signals.shift();
  if (dropped) {
    state.totalDropped += 1;
  } else {
    state.totalEmitted += 1;
    state.byMarket[out.market] = (state.byMarket[out.market] ?? 0) + 1;
    state.bySide[out.side] = (state.bySide[out.side] ?? 0) + 1;
    const kept = state.signals.filter((x) => !x.dropped);
    state.avgConfidence = kept.reduce((a, x) => a + x.confidence, 0) / Math.max(1, kept.length);
  }
  return s;
}

function logLine(s: Signal): string {
  const tag = s.dropped ? "DROP" : "SIG ";
  return `${tag}  #${s.seq} conf=${s.confidence.toFixed(2)} ${s.side} ${s.market} qty=${s.quantity.toFixed(2)} @${s.price.toFixed(4)}  ${truncate(s.prompt, 40)}`;
}

function truncate(s: string, n: number) { return s.length <= n ? s : s.slice(0, n - 1) + "…"; }

type LlmOut = { market: string; side: Side; price: number; quantity: number; confidence: number; rationale: string };
function mockLlm(prompt: string, markets: string[], mids: number[]): LlmOut {
  const p = prompt.toLowerCase();
  const bullWords = ["pump", "up", "moon", "bullish", "buy", "long", "rally", "rip"];
  const bearWords = ["dump", "down", "bearish", "sell", "short", "crash", "drop", "fall"];
  const bull = bullWords.reduce((a, w) => a + (p.includes(w) ? 1 : 0), 0);
  const bear = bearWords.reduce((a, w) => a + (p.includes(w) ? 1 : 0), 0);
  const side: Side = bear > bull ? "OFFER" : "BID";

  let idx = 0;
  for (let i = 0; i < markets.length; i++) {
    const base = markets[i].split("-")[0].toLowerCase();
    if (base.length >= 2 && p.includes(base)) { idx = i; break; }
  }
  const market = markets[idx];
  const mid = mids[idx];

  let conf = 0.55;
  if (p.includes("high confidence") || p.includes("strong")) conf += 0.25;
  if (p.includes("low confidence") || p.includes("uncertain") || p.includes("maybe")) conf -= 0.15;
  if (p.includes("hourly") || p.includes("within the hour")) conf += 0.05;
  if (p.includes("news") || p.includes("event")) conf += 0.05;
  if (bull + bear >= 3) conf += 0.05;
  conf = Math.max(0, Math.min(0.99, conf));
  conf = Math.round(conf * 100) / 100;

  const quantity = Math.min(5, 0.5 + Math.abs(bull + bear) * 0.75);
  const bias = side === "BID" ? 1.002 : 0.998;
  const price = round8(mid * bias);
  const rationale = `bull=${bull} bear=${bear} → ${side} · market=${market} · confidence adjusted`;
  return { market, side, price, quantity, confidence: conf, rationale };
}

function round8(n: number) { return Math.round(n * 1e8) / 1e8; }
