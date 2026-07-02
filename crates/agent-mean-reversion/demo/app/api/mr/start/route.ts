import { NextResponse } from "next/server";
import { startMr } from "@/lib/store";
import type { MrConfig } from "@/lib/mr-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const config: MrConfig = {
      market: String(body.market ?? "CC-USDC"),
      emaWindow: num(body.emaWindow, "emaWindow"),
      deviationPct: num(body.deviationPct, "deviationPct"),
      quantity: num(body.quantity, "quantity"),
      warmupSamples: num(body.warmupSamples, "warmupSamples"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.emaWindow < 2) return NextResponse.json({ error: "emaWindow must be >= 2" }, { status: 400 });
    if (config.deviationPct <= 0) return NextResponse.json({ error: "deviationPct must be > 0" }, { status: 400 });
    if (config.quantity <= 0) return NextResponse.json({ error: "quantity must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (config.warmupSamples < 0) return NextResponse.json({ error: "warmupSamples must be >= 0" }, { status: 400 });
    const state = startMr(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
