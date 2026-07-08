import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { AlgoConfig, Step } from "@/lib/algo-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

// Plan text format — one step per block, blank-line-separated.
// Each block starts with algorithm=... and has key=value lines.
// Examples:
//   algorithm=twap
//   market=CC-USDC side=buy total=100 slices=10 duration_secs=60 price_offset_pct=0
//
//   algorithm=iceberg
//   market=CC-USDC side=sell total=50 visible=2 price=1.02 poll_secs=3
//
//   algorithm=liquidity-seeking
//   market=BTC-USD side=buy total=5 max_chunk=0.5 max_slippage_bps=25 depth=20 poll_secs=3
function parsePlan(raw: unknown): Step[] {
  if (typeof raw !== "string") throw new Error("plan must be a text block");
  const blocks = raw.split(/\n\s*\n/).map((b) => b.trim()).filter((b) => b.length > 0);
  return blocks.map((block, i) => parseStep(block, i + 1));
}

function parseStep(block: string, idx: number): Step {
  const tokens = block.split(/\s+/).filter((t) => t.length > 0 && !t.startsWith("#"));
  const kv: Record<string, string> = {};
  for (const tok of tokens) {
    const eq = tok.indexOf("=");
    if (eq <= 0) throw new Error(`step ${idx}: bad token "${tok}"`);
    kv[tok.slice(0, eq).trim()] = tok.slice(eq + 1).trim();
  }
  const algo = kv["algorithm"];
  if (!algo) throw new Error(`step ${idx}: missing algorithm=`);
  const market = need(kv, "market", idx);
  if (!/^[A-Z0-9]+-[A-Z0-9]+$/i.test(market)) throw new Error(`step ${idx}: market "${market}" must look like BASE-QUOTE`);
  const side = (need(kv, "side", idx).toLowerCase());
  if (side !== "buy" && side !== "sell") throw new Error(`step ${idx}: side must be buy|sell`);
  const total = num(need(kv, "total", idx), `step ${idx}.total`);
  if (total <= 0) throw new Error(`step ${idx}: total must be > 0`);

  if (algo === "twap") {
    return {
      algorithm: "twap", market: market.toUpperCase(), side, total,
      slices: Math.max(1, Math.floor(num(need(kv, "slices", idx), `step ${idx}.slices`))),
      durationSecs: Math.max(1, Math.floor(num(need(kv, "duration_secs", idx), `step ${idx}.duration_secs`))),
      priceOffsetPct: num(kv["price_offset_pct"] ?? "0", `step ${idx}.price_offset_pct`),
    };
  }
  if (algo === "iceberg") {
    const visible = num(need(kv, "visible", idx), `step ${idx}.visible`);
    if (visible <= 0 || visible > total) throw new Error(`step ${idx}: visible must be in (0, total]`);
    return {
      algorithm: "iceberg", market: market.toUpperCase(), side, total, visible,
      price: num(need(kv, "price", idx), `step ${idx}.price`),
      pollSecs: Math.max(1, Math.floor(num(kv["poll_secs"] ?? "3", `step ${idx}.poll_secs`))),
    };
  }
  if (algo === "liquidity-seeking") {
    return {
      algorithm: "liquidity-seeking", market: market.toUpperCase(), side, total,
      maxChunk: num(need(kv, "max_chunk", idx), `step ${idx}.max_chunk`),
      maxSlippageBps: Math.max(1, Math.floor(num(kv["max_slippage_bps"] ?? "25", `step ${idx}.max_slippage_bps`))),
      depth: Math.max(1, Math.floor(num(kv["depth"] ?? "20", `step ${idx}.depth`))),
      pollSecs: Math.max(1, Math.floor(num(kv["poll_secs"] ?? "3", `step ${idx}.poll_secs`))),
    };
  }
  throw new Error(`step ${idx}: unknown algorithm "${algo}"`);
}

function need(kv: Record<string, string>, key: string, idx: number): string {
  const v = kv[key];
  if (v === undefined || v.length === 0) throw new Error(`step ${idx}: missing ${key}=`);
  return v;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const steps = parsePlan(body.plan);
    if (steps.length === 0) return NextResponse.json({ error: "plan has zero steps" }, { status: 400 });
    if (steps.length > 10) return NextResponse.json({ error: "at most 10 steps per plan in this demo" }, { status: 400 });
    const startingPrice = num(body.startingPrice, "startingPrice");
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const bookVolatility = num(body.bookVolatility, "bookVolatility");
    if (bookVolatility < 0 || bookVolatility > 0.1) return NextResponse.json({ error: "bookVolatility in [0, 0.1]" }, { status: 400 });

    const config: AlgoConfig = { steps, startingPrice, bookVolatility };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
