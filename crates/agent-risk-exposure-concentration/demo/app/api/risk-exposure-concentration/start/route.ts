import { NextResponse } from "next/server";
import { startRiskExposure } from "@/lib/store";
import type { Instrument, RiskExposureConfig } from "@/lib/riskexposure-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseMarkets(raw: unknown): string[] {
  const s = String(raw ?? "").trim();
  if (!s) return [];
  return s.split(",").map((m) => m.trim()).filter((m) => m.length > 0);
}

function parseInstruments(raw: unknown): Instrument[] {
  const s = String(raw ?? "").trim();
  if (!s) return [];
  return s
    .split(",")
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0)
    .map((entry) => {
      const parts = entry.split(":").map((p) => p.trim());
      if (parts.length < 2 || parts.length > 3) {
        throw new Error(`instrument entry '${entry}' must be name:balance[:priceMultiplier]`);
      }
      const [name, balStr, multStr] = parts;
      if (!name) throw new Error(`instrument entry '${entry}' has empty name`);
      const balance = num(balStr, `instrument ${name} balance`);
      if (balance < 0) throw new Error(`instrument ${name} balance must be >= 0`);
      const priceMultiplier = multStr === undefined ? 1 : num(multStr, `instrument ${name} priceMultiplier`);
      if (priceMultiplier < 0) throw new Error(`instrument ${name} priceMultiplier must be >= 0`);
      return { name, balance, priceMultiplier };
    });
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const markets = parseMarkets(body.markets);
    if (markets.length === 0) return NextResponse.json({ error: "markets is required (comma-separated)" }, { status: 400 });
    const instruments = parseInstruments(body.instruments);
    if (instruments.length === 0) return NextResponse.json({ error: "instruments is required (comma-separated name:balance:mult)" }, { status: 400 });
    const concentrationWarnPct = num(body.concentrationWarnPct, "concentrationWarnPct");
    if (concentrationWarnPct <= 0 || concentrationWarnPct > 100) return NextResponse.json({ error: "concentrationWarnPct must be in (0, 100]" }, { status: 400 });
    const snapshotIntervalSecs = num(body.snapshotIntervalSecs, "snapshotIntervalSecs");
    if (snapshotIntervalSecs <= 0) return NextResponse.json({ error: "snapshotIntervalSecs must be > 0" }, { status: 400 });
    const startingPrice = num(body.startingPrice, "startingPrice");
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });

    const config: RiskExposureConfig = {
      markets,
      instruments,
      concentrationWarnPct,
      snapshotIntervalSecs,
      startingPrice,
    };
    const state = startRiskExposure(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
