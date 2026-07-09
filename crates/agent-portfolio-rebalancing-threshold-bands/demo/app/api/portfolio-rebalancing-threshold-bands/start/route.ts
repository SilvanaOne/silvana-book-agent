import { NextResponse } from "next/server";
import { startPortfolioRebalancing } from "@/lib/store";
import type { PortfolioRebalancingConfig, Target } from "@/lib/portfoliorebalancing-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseTargets(raw: unknown): Target[] {
  if (!Array.isArray(raw)) throw new Error("targets must be an array");
  if (raw.length === 0) throw new Error("targets must not be empty");
  const out: Target[] = [];
  for (let i = 0; i < raw.length; i++) {
    const t = raw[i] as Record<string, unknown>;
    const instrument = String(t.instrument ?? "").trim();
    const market = String(t.market ?? "").trim();
    if (!instrument) throw new Error(`targets[${i}].instrument required`);
    if (!market) throw new Error(`targets[${i}].market required`);
    const targetWeight = num(t.targetWeight, `targets[${i}].targetWeight`);
    const startBalance = num(t.startBalance, `targets[${i}].startBalance`);
    if (targetWeight < 0 || targetWeight > 1) {
      throw new Error(`targets[${i}].targetWeight must be in [0, 1]`);
    }
    if (startBalance < 0) throw new Error(`targets[${i}].startBalance must be >= 0`);
    out.push({ instrument, market, targetWeight, startBalance });
  }
  return out;
}

export async function POST(req: Request): Promise<Response> {
  try {
    const body = (await req.json()) as Record<string, unknown>;
    const targets = parseTargets(body.targets);
    const sum = targets.reduce((s, t) => s + t.targetWeight, 0);
    if (Math.abs(sum - 1) > 0.01) {
      return NextResponse.json(
        { error: `target weights must sum to 1.0 (+/- 0.01), got ${sum.toFixed(4)}` },
        { status: 400 },
      );
    }
    const upperBandPct = num(body.upperBandPct, "upperBandPct");
    const lowerBandPct = num(body.lowerBandPct, "lowerBandPct");
    const priceOffsetPct = num(body.priceOffsetPct, "priceOffsetPct");
    const checkIntervalSecs = num(body.checkIntervalSecs, "checkIntervalSecs");
    const startingPrice = num(body.startingPrice, "startingPrice");
    if (upperBandPct <= 0) return NextResponse.json({ error: "upperBandPct must be > 0" }, { status: 400 });
    if (lowerBandPct <= 0) return NextResponse.json({ error: "lowerBandPct must be > 0" }, { status: 400 });
    if (!Number.isFinite(priceOffsetPct)) return NextResponse.json({ error: "priceOffsetPct must be finite" }, { status: 400 });
    if (checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });

    const totalStart = targets.reduce((s, t) => s + t.startBalance, 0);
    if (totalStart <= 0) return NextResponse.json({ error: "at least one target must have startBalance > 0" }, { status: 400 });

    const config: PortfolioRebalancingConfig = {
      targets,
      upperBandPct,
      lowerBandPct,
      priceOffsetPct,
      checkIntervalSecs,
      startingPrice,
    };
    const state = startPortfolioRebalancing(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
