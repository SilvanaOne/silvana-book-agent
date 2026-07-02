import { NextResponse } from "next/server";
import { startWatchlist } from "@/lib/store";
import type { WatchlistConfig } from "@/lib/watchlist-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseMarkets(v: unknown): string[] {
  if (Array.isArray(v)) return v.map((x) => String(x).trim()).filter(Boolean);
  return String(v ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const markets = parseMarkets(body.markets);
    if (markets.length === 0) return NextResponse.json({ error: "markets must be non-empty" }, { status: 400 });

    const config: WatchlistConfig = {
      markets,
      depthLevels: num(body.depthLevels, "depthLevels"),
      includeOrderbook: body.includeOrderbook !== false,
      includePrices: body.includePrices !== false,
      pollSecs: num(body.pollSecs, "pollSecs"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (!Number.isInteger(config.depthLevels) || config.depthLevels < 1) {
      return NextResponse.json({ error: "depthLevels must be an integer >= 1" }, { status: 400 });
    }
    if (config.pollSecs <= 0) return NextResponse.json({ error: "pollSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (!config.includePrices && !config.includeOrderbook) {
      return NextResponse.json({ error: "at least one of includePrices / includeOrderbook must be true" }, { status: 400 });
    }
    const state = startWatchlist(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
