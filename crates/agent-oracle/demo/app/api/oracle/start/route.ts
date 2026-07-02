import { NextResponse } from "next/server";
import { startOracle } from "@/lib/store";
import type { OracleConfig, OracleSource } from "@/lib/oracle-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const ALLOWED_SOURCES: readonly OracleSource[] = ["binance_spot", "bybit", "coingecko"] as const;

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const marketsRaw = body.markets;
    let markets: string[] = [];
    if (Array.isArray(marketsRaw)) {
      markets = marketsRaw.map((m) => String(m).trim()).filter((m) => m.length > 0);
    } else if (typeof marketsRaw === "string") {
      markets = marketsRaw.split(",").map((m) => m.trim()).filter((m) => m.length > 0);
    }
    if (markets.length === 0) {
      return NextResponse.json({ error: "markets is required and must not be empty" }, { status: 400 });
    }

    const sourceRaw = String(body.source ?? "binance_spot") as OracleSource;
    if (!ALLOWED_SOURCES.includes(sourceRaw)) {
      return NextResponse.json({ error: `source must be one of ${ALLOWED_SOURCES.join(", ")}` }, { status: 400 });
    }

    const pollSecs = num(body.pollSecs, "pollSecs");
    if (pollSecs <= 0) return NextResponse.json({ error: "pollSecs must be > 0" }, { status: 400 });

    // Starting price(s): either a single value applied to all markets or an array.
    let startingPrices: number[] = [];
    const spRaw = body.startingPrices ?? body.startingPrice;
    if (Array.isArray(spRaw)) {
      startingPrices = spRaw.map((v, i) => num(v, `startingPrices[${i}]`));
    } else {
      const sp = num(spRaw, "startingPrice");
      startingPrices = markets.map(() => sp);
    }
    if (startingPrices.length !== markets.length) {
      // Broadcast single value; otherwise error.
      if (startingPrices.length === 1) {
        startingPrices = markets.map(() => startingPrices[0]);
      } else {
        return NextResponse.json(
          { error: `startingPrices length (${startingPrices.length}) must match markets length (${markets.length})` },
          { status: 400 },
        );
      }
    }
    if (startingPrices.some((p) => !(p > 0))) {
      return NextResponse.json({ error: "startingPrices must all be > 0" }, { status: 400 });
    }

    const config: OracleConfig = {
      markets,
      source: sourceRaw,
      pollSecs: Math.floor(pollSecs),
      startingPrices,
    };
    const state = startOracle(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
