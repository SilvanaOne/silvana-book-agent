import { NextResponse } from "next/server";
import { startPortfolioHealth } from "@/lib/store";
import type { PortfolioHealthConfig, PortfolioHealthInstrument } from "@/lib/portfoliohealth-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseMarkets(v: unknown): string[] {
  if (Array.isArray(v)) return v.map(String).map((s) => s.trim()).filter(Boolean);
  if (typeof v === "string") return v.split(",").map((s) => s.trim()).filter(Boolean);
  return [];
}

function parseInstruments(v: unknown): PortfolioHealthInstrument[] {
  // Accept either "Amulet:50,CBTC:0.5,CETH:5" or an array of {name, startBalance}.
  if (Array.isArray(v)) {
    return v.map((row) => {
      const name = String((row as { name: unknown }).name ?? "").trim();
      const bal = num((row as { startBalance: unknown }).startBalance, `instrument ${name} startBalance`);
      if (!name) throw new Error("instrument name is required");
      if (bal < 0) throw new Error(`instrument ${name} startBalance must be >= 0`);
      return { name, startBalance: bal };
    });
  }
  if (typeof v === "string") {
    return v
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean)
      .map((entry) => {
        const [rawName, rawBal] = entry.split(":");
        const name = (rawName ?? "").trim();
        if (!name) throw new Error(`invalid instrument entry '${entry}'`);
        const bal = num(rawBal, `instrument ${name} startBalance`);
        if (bal < 0) throw new Error(`instrument ${name} startBalance must be >= 0`);
        return { name, startBalance: bal };
      });
  }
  return [];
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const markets = parseMarkets(body.markets);
    const instruments = parseInstruments(body.instruments);
    const snapshotIntervalSecs = num(body.snapshotIntervalSecs, "snapshotIntervalSecs");
    const startingPrice = num(body.startingPrice, "startingPrice");

    if (markets.length === 0) return NextResponse.json({ error: "markets must not be empty" }, { status: 400 });
    if (instruments.length === 0) return NextResponse.json({ error: "instruments must not be empty" }, { status: 400 });
    if (snapshotIntervalSecs <= 0) return NextResponse.json({ error: "snapshotIntervalSecs must be > 0" }, { status: 400 });
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });

    const config: PortfolioHealthConfig = { markets, instruments, snapshotIntervalSecs, startingPrice };
    const state = startPortfolioHealth(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
