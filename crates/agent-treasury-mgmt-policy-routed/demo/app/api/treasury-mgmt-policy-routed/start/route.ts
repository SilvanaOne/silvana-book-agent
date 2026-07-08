import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { Target, TreasuryConfig } from "@/lib/treasury-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

// Targets text (one per line):
//   instrument=Amulet market=CC-USDC target=10000 balance=8500 price=1.0
function parseTargets(raw: unknown): { targets: Target[]; balances: Record<string, number>; prices: Record<string, number> } {
  if (typeof raw !== "string") throw new Error("targets must be text");
  const balances: Record<string, number> = {};
  const prices: Record<string, number> = {};
  const targets: Target[] = [];
  for (const line of raw.split("\n").map((l) => l.trim()).filter((l) => l.length > 0 && !l.startsWith("#"))) {
    const kv = Object.fromEntries(line.split(/\s+/).map((tok) => { const eq = tok.indexOf("="); return eq >= 0 ? [tok.slice(0, eq), tok.slice(eq + 1)] : [tok, ""]; })) as Record<string, string>;
    const instrument = kv["instrument"];
    const market = kv["market"];
    if (!instrument || !market) throw new Error(`bad target line: ${line}`);
    if (!/^[A-Z0-9]+-[A-Z0-9]+$/i.test(market)) throw new Error(`market "${market}" must look like BASE-QUOTE`);
    const targetValue = num(kv["target"] ?? "0", "target");
    if (targetValue <= 0) throw new Error(`target for ${instrument} must be > 0`);
    targets.push({ instrument, market: market.toUpperCase(), targetValue });
    if (kv["balance"]) balances[instrument] = num(kv["balance"], `${instrument}.balance`);
    if (kv["price"]) prices[market.toUpperCase()] = num(kv["price"], `${market}.price`);
  }
  if (targets.length === 0) throw new Error("no targets defined");
  return { targets, balances, prices };
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const { targets, balances, prices } = parseTargets(body.targets);
    const approvalThresholdQuote = num(body.approvalThresholdQuote, "approvalThresholdQuote");
    const maxTradeQuote = num(body.maxTradeQuote, "maxTradeQuote");
    const maxDailyTradeQuote = num(body.maxDailyTradeQuote, "maxDailyTradeQuote");
    if (approvalThresholdQuote <= 0 || maxTradeQuote <= 0 || maxDailyTradeQuote <= 0) throw new Error("quote thresholds must be > 0");
    const thresholdQuote = num(body.thresholdQuote, "thresholdQuote");
    if (thresholdQuote <= 0) throw new Error("thresholdQuote must be > 0");
    const rebalanceFraction = num(body.rebalanceFraction, "rebalanceFraction");
    if (rebalanceFraction <= 0 || rebalanceFraction > 1) throw new Error("rebalanceFraction in (0, 1]");
    const checkIntervalSecs = Math.max(1, Math.min(300, Math.floor(num(body.checkIntervalSecs, "checkIntervalSecs"))));
    const balanceDriftPerCycle = num(body.balanceDriftPerCycle, "balanceDriftPerCycle");

    const config: TreasuryConfig = {
      targets, approvalThresholdQuote, maxTradeQuote, maxDailyTradeQuote,
      thresholdQuote, rebalanceFraction, checkIntervalSecs,
      startingBalances: balances, startingPrices: prices, balanceDriftPerCycle,
    };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
