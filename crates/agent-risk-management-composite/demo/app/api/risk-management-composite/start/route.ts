import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { Policy, RiskConfig } from "@/lib/riskmgmt-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseMarkets(raw: unknown): string[] {
  if (typeof raw !== "string") throw new Error("markets must be a comma-separated string");
  const list = raw.split(",").map((s) => s.trim().toUpperCase()).filter((s) => s.length > 0);
  if (list.length === 0) throw new Error("at least one market is required");
  if (list.length > 6) throw new Error("at most 6 markets are supported in this demo");
  for (const m of list) if (!/^[A-Z0-9]+-[A-Z0-9]+$/.test(m)) throw new Error(`market "${m}" must look like BASE-QUOTE`);
  return list;
}

function parseStartingPrices(raw: unknown, count: number): number[] {
  if (typeof raw !== "string") throw new Error("startingPrices must be a comma-separated string");
  const parts = raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0);
  if (parts.length === 0) throw new Error("startingPrices required");
  const nums = parts.map((p, i) => {
    const n = Number(p);
    if (!Number.isFinite(n) || n <= 0) throw new Error(`startingPrices[${i}] must be > 0`);
    return n;
  });
  while (nums.length < count) nums.push(nums[nums.length - 1]);
  return nums.slice(0, count);
}

function parsePerMarket(raw: unknown): Record<string, string> {
  if (typeof raw !== "string" || raw.trim().length === 0) return {};
  const out: Record<string, string> = {};
  for (const entry of raw.split(/[\n,]/).map((s) => s.trim()).filter((s) => s.length > 0)) {
    const [k, v] = entry.split("=").map((s) => s.trim());
    if (!k || !v) throw new Error(`perMarketMaxNotional entry "${entry}" must be MARKET=DECIMAL`);
    const n = Number(v);
    if (!Number.isFinite(n) || n <= 0) throw new Error(`perMarketMaxNotional[${k}] must be > 0`);
    out[k.toUpperCase()] = v;
  }
  return out;
}

function optNum(v: unknown, name: string): number | undefined {
  if (v === "" || v === null || v === undefined) return undefined;
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function optString(v: unknown, name: string): string | undefined {
  if (v === "" || v === null || v === undefined) return undefined;
  if (typeof v !== "string") throw new Error(`${name} must be a string`);
  const n = Number(v);
  if (!Number.isFinite(n) || n <= 0) throw new Error(`${name} must be > 0`);
  return v.trim();
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const markets = parseMarkets(body.markets);
    const startingPrices = parseStartingPrices(body.startingPrices, markets.length);
    const checkIntervalSecs = Math.max(1, Math.min(300, Math.floor(num(body.checkIntervalSecs, "checkIntervalSecs"))));
    const spawnPerCycle = Math.max(0, Math.min(50, num(body.spawnPerCycle, "spawnPerCycle")));
    const enforce = Boolean(body.enforce);

    const policy: Policy = {
      maxOpenOrders: optNum(body.maxOpenOrders, "maxOpenOrders"),
      maxOpenNotional: optString(body.maxOpenNotional, "maxOpenNotional"),
      maxPendingSettlements: optNum(body.maxPendingSettlements, "maxPendingSettlements"),
      maxFailedSettlements: optNum(body.maxFailedSettlements, "maxFailedSettlements"),
      perMarketMaxNotional: parsePerMarket(body.perMarketMaxNotional),
    };

    const config: RiskConfig = {
      markets,
      startingPrices,
      policy,
      enforce,
      checkIntervalSecs,
      spawnPerCycle,
    };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
