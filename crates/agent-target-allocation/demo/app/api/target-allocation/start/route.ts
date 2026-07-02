import { NextResponse } from "next/server";
import { startTargetAllocation } from "@/lib/store";
import { DEFAULT_TARGETS, type Target, type TargetAllocationConfig } from "@/lib/targetallocation-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseTargets(raw: unknown): Target[] {
  let arr: unknown = raw;
  if (typeof raw === "string") {
    const s = raw.trim();
    if (!s) throw new Error("targets must not be empty");
    try {
      arr = JSON.parse(s);
    } catch (e) {
      throw new Error(`targets: invalid JSON (${(e as Error).message})`);
    }
  }
  if (!Array.isArray(arr) || arr.length === 0) throw new Error("targets must be a non-empty array");
  const out: Target[] = [];
  for (let i = 0; i < arr.length; i++) {
    const t = arr[i] as Record<string, unknown>;
    if (!t || typeof t !== "object") throw new Error(`targets[${i}] must be an object`);
    const instrument = String(t.instrument ?? "").trim();
    const market = String(t.market ?? "").trim();
    if (!instrument) throw new Error(`targets[${i}].instrument is required`);
    if (!market) throw new Error(`targets[${i}].market is required`);
    const targetQuote = num(t.targetQuote, `targets[${i}].targetQuote`);
    const startBalance = num(t.startBalance ?? 0, `targets[${i}].startBalance`);
    const priceMultiplier = num(t.priceMultiplier ?? 1, `targets[${i}].priceMultiplier`);
    if (targetQuote <= 0) throw new Error(`targets[${i}].targetQuote must be > 0`);
    if (priceMultiplier <= 0) throw new Error(`targets[${i}].priceMultiplier must be > 0`);
    if (startBalance < 0) throw new Error(`targets[${i}].startBalance must be >= 0`);
    out.push({ instrument, market, targetQuote, startBalance, priceMultiplier });
  }
  return out;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const targets = body.targets !== undefined ? parseTargets(body.targets) : [...DEFAULT_TARGETS];
    const thresholdQuote = num(body.thresholdQuote ?? 100, "thresholdQuote");
    const rebalanceFraction = num(body.rebalanceFraction ?? 0.5, "rebalanceFraction");
    const checkIntervalSecs = num(body.checkIntervalSecs ?? 5, "checkIntervalSecs");
    const startingPrice = num(body.startingPrice ?? 0.15, "startingPrice");

    if (thresholdQuote <= 0) return NextResponse.json({ error: "thresholdQuote must be > 0" }, { status: 400 });
    if (rebalanceFraction <= 0 || rebalanceFraction > 1) return NextResponse.json({ error: "rebalanceFraction must be in (0, 1]" }, { status: 400 });
    if (checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });

    const config: TargetAllocationConfig = { targets, thresholdQuote, rebalanceFraction, checkIntervalSecs, startingPrice };
    const state = startTargetAllocation(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
