// Port of agent-strategy-genesis. Compiles a natural-language spec into
// an algo-order TOML step. Same rules as
// crates/agent-strategy-genesis/src/main.rs::compile_step.

export type Algorithm = "twap" | "iceberg" | "liquidity-seeking";

export type Step = Readonly<{
  seq: number;
  t: number;
  spec: string;
  algorithm: Algorithm;
  market: string;
  side: "buy" | "sell";
  total: string;
  kv: Array<[string, string]>;
  toml: string;
  error?: string;
}>;

export type GenesisState = {
  status: "loaded";
  steps: Step[];              // ~40
  totalCompiled: number;
  totalErrored: number;
  byAlgo: Record<string, number>;
  bySide: Record<string, number>;
  byMarket: Record<string, number>;
  nextSeq: number;
};

const MAX_STEPS = 40;

export function initState(): GenesisState {
  return { status: "loaded", steps: [], totalCompiled: 0, totalErrored: 0, byAlgo: {}, bySide: {}, byMarket: {}, nextSeq: 1 };
}

export function compile(state: GenesisState, spec: string, now: number): Step {
  const seq = state.nextSeq++;
  try {
    const parsed = compileStep(spec);
    const toml = toToml(parsed);
    const s: Step = { seq, t: now, spec, ...parsed, toml };
    state.steps.push(s);
    while (state.steps.length > MAX_STEPS) state.steps.shift();
    state.totalCompiled += 1;
    state.byAlgo[parsed.algorithm] = (state.byAlgo[parsed.algorithm] ?? 0) + 1;
    state.bySide[parsed.side] = (state.bySide[parsed.side] ?? 0) + 1;
    state.byMarket[parsed.market] = (state.byMarket[parsed.market] ?? 0) + 1;
    return s;
  } catch (e) {
    const s: Step = {
      seq, t: now, spec,
      algorithm: "twap", market: "?", side: "buy", total: "0", kv: [],
      toml: `# error: ${(e as Error).message}\n`,
      error: (e as Error).message,
    };
    state.steps.push(s);
    while (state.steps.length > MAX_STEPS) state.steps.shift();
    state.totalErrored += 1;
    return s;
  }
}

type Parsed = Omit<Step, "seq" | "t" | "spec" | "toml" | "error">;

function compileStep(spec: string): Parsed {
  const s = spec.toLowerCase();
  const side: "buy" | "sell" = (s.includes("sell") || s.includes("offer")) ? "sell" : "buy";
  const market = extractMarket(spec);
  if (!market) throw new Error("could not find a BASE-QUOTE market in the spec");
  const total = extractTotal(s);
  if (!total) throw new Error("could not find a total quantity in the spec");

  const algorithm: Algorithm =
    (s.includes("iceberg") || s.includes("visible")) ? "iceberg"
    : (s.includes("liquid") || s.includes("slippage")) ? "liquidity-seeking"
    : "twap";

  const kv: Array<[string, string]> = [];
  if (algorithm === "twap") {
    kv.push(["slices", extractNearNumber(s, "slices") ?? "10"]);
    kv.push(["duration_secs", extractDurationSecs(s) ?? "600"]);
    const offset = extractNearNumber(s, "offset");
    if (offset) kv.push(["price_offset_pct", offset]);
  } else if (algorithm === "iceberg") {
    kv.push(["visible", extractNearNumber(s, "visible") ?? "1"]);
    kv.push(["price", extractNearNumber(s, "price") ?? "0"]);
    const poll = extractNearNumber(s, "poll");
    if (poll) kv.push(["poll_secs", poll]);
  } else {
    kv.push(["max_chunk", extractNearNumber(s, "chunk") ?? "1"]);
    kv.push(["max_slippage_bps", extractNearNumber(s, "bps") ?? extractNearNumber(s, "slippage") ?? "25"]);
    kv.push(["depth", extractNearNumber(s, "depth") ?? "20"]);
  }
  return { algorithm, market, side, total, kv };
}

function extractMarket(spec: string): string | null {
  for (const rawTok of spec.split(/\s+/)) {
    const t = rawTok.replace(/[^A-Za-z0-9-]/g, "");
    const parts = t.split("-");
    if (parts.length === 2 && parts[0] && parts[1] && /^[A-Za-z0-9]+$/.test(parts[0]) && /^[A-Za-z0-9]+$/.test(parts[1])) {
      return t.toUpperCase();
    }
  }
  return null;
}

function extractTotal(s: string): string | null {
  const tokens = s.split(/\s+/);
  const skipNext = ["hour", "hours", "minute", "minutes", "second", "seconds", "bps", "slices", "slice", "visible", "price", "poll", "depth", "chunk", "offset", "%", "slippage"];
  for (let i = 0; i < tokens.length; i++) {
    const cleaned = tokens[i].replace(/[^0-9.]/g, "");
    if (cleaned.length === 0) continue;
    const n = Number(cleaned);
    if (!Number.isFinite(n) || n <= 0) continue;
    const next = tokens[i + 1] ?? "";
    if (skipNext.some((k) => next.includes(k))) continue;
    return cleaned;
  }
  return null;
}

function extractNearNumber(s: string, keyword: string): string | null {
  const tokens = s.split(/\s+/);
  for (let i = 0; i < tokens.length; i++) {
    if (!tokens[i].includes(keyword)) continue;
    // Prefer token AFTER the keyword.
    for (const j of [i + 1, i - 1]) {
      if (j < 0 || j >= tokens.length) continue;
      const cleaned = tokens[j].replace(/[^0-9.]/g, "");
      if (cleaned.length === 0) continue;
      const n = Number(cleaned);
      if (Number.isFinite(n) && n >= 0) return cleaned;
    }
  }
  return null;
}

function extractDurationSecs(s: string): string | null {
  const tokens = s.split(/\s+/);
  for (let i = 0; i < tokens.length - 1; i++) {
    const cleaned = tokens[i].replace(/[^0-9.]/g, "");
    const n = Number(cleaned);
    if (!Number.isFinite(n) || n <= 0) continue;
    const unit = tokens[i + 1];
    let secs = 0;
    if (unit.startsWith("hour")) secs = n * 3600;
    else if (unit.startsWith("minute") || unit.startsWith("min")) secs = n * 60;
    else if (unit.startsWith("second") || unit.startsWith("sec") || unit === "s") secs = n;
    else continue;
    if (secs > 0) return Math.round(secs).toString();
  }
  return null;
}

function toToml(p: Parsed): string {
  const num = (v: string) => /^-?\d+(\.\d+)?$/.test(v);
  let s = "[[step]]\n";
  s += `algorithm = "${p.algorithm}"\n`;
  s += `market = "${p.market}"\n`;
  s += `side = "${p.side}"\n`;
  s += `total = "${p.total}"\n`;
  for (const [k, v] of p.kv) {
    s += num(v) ? `${k} = ${v}\n` : `${k} = "${v}"\n`;
  }
  return s;
}
