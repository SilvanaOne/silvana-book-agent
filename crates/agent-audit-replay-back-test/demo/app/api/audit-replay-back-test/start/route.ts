import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { ReplayConfig, Rules } from "@/lib/replay-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}
function optNum(v: unknown, name: string): number | undefined {
  if (v === undefined || v === null || (typeof v === "string" && v.trim().length === 0)) return undefined;
  return num(v, name);
}
function parseList(raw: unknown, upper = false): string[] {
  if (typeof raw !== "string") return [];
  return raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0).map((s) => upper ? s.toUpperCase() : s);
}
function parseCapMap(raw: unknown): Record<string, number> {
  const out: Record<string, number> = {};
  if (typeof raw !== "string") return out;
  for (const entry of raw.split(/[\n,]/).map((s) => s.trim()).filter((s) => s.length > 0)) {
    const [k, v] = entry.split("=").map((s) => s.trim());
    if (!k || !v) throw new Error(`bad per-market cap entry: ${entry}`);
    const n = Number(v);
    if (!Number.isFinite(n) || n <= 0) throw new Error(`per-market cap for ${k} must be > 0`);
    out[k.toUpperCase()] = n;
  }
  return out;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const historySize = Math.max(20, Math.min(500, Math.floor(num(body.historySize, "historySize"))));
    const parties = parseList(body.parties);
    if (parties.length === 0) throw new Error("parties pool required");
    const markets = parseList(body.markets, true);
    if (markets.length === 0) throw new Error("markets pool required");
    const emitAccepts = Boolean(body.emitAccepts);
    const rules: Rules = {
      maxNotionalPerOrder: optNum(body.maxNotionalPerOrder, "maxNotionalPerOrder"),
      maxQuantityPerOrder: optNum(body.maxQuantityPerOrder, "maxQuantityPerOrder"),
      minPrice: optNum(body.minPrice, "minPrice"),
      maxPrice: optNum(body.maxPrice, "maxPrice"),
      blockedMarkets: parseList(body.blockedMarkets, true),
      allowedMarkets: parseList(body.allowedMarkets, true),
      allowedSides: parseList(body.allowedSides).map((s) => s.toLowerCase()),
      perMarketMaxDailyNotional: parseCapMap(body.perMarketMaxDailyNotional),
    };
    const config: ReplayConfig = { historySize, parties, markets, rules, emitAccepts };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
