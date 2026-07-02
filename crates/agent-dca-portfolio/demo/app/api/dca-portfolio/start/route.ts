import { NextResponse } from "next/server";
import { startDcaPortfolio } from "@/lib/store";
import type { DcaPortfolioConfig, Side } from "@/lib/dcaportfolio-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function optNum(v: unknown, name: string): number | null {
  if (v === null || v === undefined || v === "") return null;
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number or empty`);
  return n;
}

function parseMarkets(v: unknown): string[] {
  if (Array.isArray(v)) return v.map((s) => String(s).trim()).filter(Boolean);
  if (typeof v === "string") return v.split(",").map((s) => s.trim()).filter(Boolean);
  return [];
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const markets = parseMarkets(body.markets);
    if (markets.length === 0) {
      return NextResponse.json({ error: "markets must be a non-empty comma-separated list" }, { status: 400 });
    }

    const side = String(body.side ?? "buy").toLowerCase() as Side;
    if (side !== "buy" && side !== "sell") {
      return NextResponse.json({ error: "side must be 'buy' or 'sell'" }, { status: 400 });
    }

    const amountPerOrder = num(body.amountPerOrder, "amountPerOrder");
    if (amountPerOrder <= 0) return NextResponse.json({ error: "amountPerOrder must be > 0" }, { status: 400 });

    const intervalSecs = num(body.intervalSecs, "intervalSecs");
    if (intervalSecs <= 0) return NextResponse.json({ error: "intervalSecs must be > 0" }, { status: 400 });

    const priceOffsetPct = num(body.priceOffsetPct, "priceOffsetPct");

    const maxTotal = optNum(body.maxTotal, "maxTotal");
    if (maxTotal !== null && maxTotal <= 0) {
      return NextResponse.json({ error: "maxTotal must be > 0 or empty" }, { status: 400 });
    }

    const startingPriceSingle = optNum(body.startingPrice, "startingPrice");
    let startingPrices: number[] = [];
    if (Array.isArray(body.startingPrices)) {
      startingPrices = body.startingPrices.map((v, i) => num(v, `startingPrices[${i}]`));
    } else if (startingPriceSingle !== null) {
      startingPrices = markets.map(() => startingPriceSingle);
    } else {
      startingPrices = markets.map(() => 0.15);
    }
    if (startingPrices.length !== markets.length) {
      // Pad or truncate to match
      if (startingPrices.length < markets.length) {
        const fill = startingPrices[startingPrices.length - 1] ?? 0.15;
        while (startingPrices.length < markets.length) startingPrices.push(fill);
      } else {
        startingPrices = startingPrices.slice(0, markets.length);
      }
    }
    for (const p of startingPrices) {
      if (!(p > 0)) return NextResponse.json({ error: "all starting prices must be > 0" }, { status: 400 });
    }

    const config: DcaPortfolioConfig = {
      markets,
      side,
      amountPerOrder,
      intervalSecs,
      priceOffsetPct,
      maxTotal,
      startingPrices,
    };
    const state = startDcaPortfolio(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
