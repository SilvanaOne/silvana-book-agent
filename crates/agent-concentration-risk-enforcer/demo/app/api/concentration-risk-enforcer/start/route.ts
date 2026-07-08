import { NextResponse } from "next/server";
import { startConcentrationRisk } from "@/lib/store";
import type { ConcentrationRiskConfig, Instrument } from "@/lib/concentrationrisk-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseInstruments(raw: unknown): Instrument[] {
  let arr: unknown;
  if (typeof raw === "string") {
    try { arr = JSON.parse(raw); } catch (e) { throw new Error(`instruments JSON parse error: ${(e as Error).message}`); }
  } else {
    arr = raw;
  }
  if (!Array.isArray(arr)) throw new Error("instruments must be a JSON array");
  if (arr.length === 0) throw new Error("instruments must not be empty");
  return arr.map((entry, idx) => {
    if (typeof entry !== "object" || entry === null) throw new Error(`instruments[${idx}] must be an object`);
    const e = entry as Record<string, unknown>;
    const name = String(e.name ?? "");
    const market = String(e.market ?? "");
    if (!name) throw new Error(`instruments[${idx}].name required`);
    if (!market) throw new Error(`instruments[${idx}].market required`);
    return {
      name,
      market,
      balance: num(e.balance, `instruments[${idx}].balance`),
      priceMultiplier: num(e.priceMultiplier, `instruments[${idx}].priceMultiplier`),
      bidsActive: Math.max(0, Math.floor(num(e.bidsActive ?? 0, `instruments[${idx}].bidsActive`))),
      offersActive: Math.max(0, Math.floor(num(e.offersActive ?? 0, `instruments[${idx}].offersActive`))),
    };
  });
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const instruments = parseInstruments(body.instrumentsJson ?? body.instruments);
    const maxSharePct = num(body.maxSharePct, "maxSharePct");
    const minSharePct = num(body.minSharePct, "minSharePct");
    const checkIntervalSecs = num(body.checkIntervalSecs, "checkIntervalSecs");
    const balanceDriftPerTick = num(body.balanceDriftPerTick, "balanceDriftPerTick");
    const startingPrice = num(body.startingPrice, "startingPrice");
    const dryRun = Boolean(body.dryRun);

    if (maxSharePct <= 0 || maxSharePct > 100) return NextResponse.json({ error: "maxSharePct must be in (0, 100]" }, { status: 400 });
    if (minSharePct < 0 || minSharePct >= maxSharePct) return NextResponse.json({ error: "minSharePct must be in [0, maxSharePct)" }, { status: 400 });
    if (checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (balanceDriftPerTick < 0) return NextResponse.json({ error: "balanceDriftPerTick must be >= 0" }, { status: 400 });
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });

    const config: ConcentrationRiskConfig = {
      instruments,
      maxSharePct,
      minSharePct,
      checkIntervalSecs,
      dryRun,
      balanceDriftPerTick,
      startingPrice,
    };
    const state = startConcentrationRisk(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
