import { NextResponse } from "next/server";
import { startTwap } from "@/lib/store";
import type { TwapConfig } from "@/lib/twap-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function optNum(v: unknown, name: string): number | null {
  if (v === undefined || v === null || v === "") return null;
  return num(v, name);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const sideRaw = String(body.side ?? "buy").toLowerCase();
    if (sideRaw !== "buy" && sideRaw !== "sell") {
      return NextResponse.json({ error: "side must be 'buy' or 'sell'" }, { status: 400 });
    }
    const slicesNum = num(body.slices, "slices");
    if (!Number.isInteger(slicesNum) || slicesNum < 1) {
      return NextResponse.json({ error: "slices must be an integer >= 1" }, { status: 400 });
    }
    const config: TwapConfig = {
      market: String(body.market ?? "CC-USDC"),
      side: sideRaw,
      total: num(body.total, "total"),
      slices: slicesNum,
      durationSecs: num(body.durationSecs, "durationSecs"),
      priceOffsetPct: num(body.priceOffsetPct, "priceOffsetPct"),
      limitPrice: optNum(body.limitPrice, "limitPrice"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.total <= 0) return NextResponse.json({ error: "total must be > 0" }, { status: 400 });
    if (config.durationSecs <= 0) return NextResponse.json({ error: "durationSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (config.limitPrice !== null && config.limitPrice <= 0) {
      return NextResponse.json({ error: "limitPrice must be > 0 when set" }, { status: 400 });
    }
    const state = startTwap(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
