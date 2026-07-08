import { NextResponse } from "next/server";
import { startSignalBot } from "@/lib/store";
import type { SignalBotConfig } from "@/lib/signalbot-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function bool(v: unknown): boolean {
  if (typeof v === "boolean") return v;
  if (typeof v === "string") return v.toLowerCase() === "true";
  return Boolean(v);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const config: SignalBotConfig = {
      signalsFilePath: String(body.signalsFilePath ?? "signals.jsonl"),
      fromEnd: bool(body.fromEnd),
      dryRun: bool(body.dryRun),
      signalArrivalPerTick: num(body.signalArrivalPerTick, "signalArrivalPerTick"),
      startingPrice: num(body.startingPrice, "startingPrice"),
      rejectionRate: num(body.rejectionRate, "rejectionRate"),
    };
    if (!config.signalsFilePath.trim()) {
      return NextResponse.json({ error: "signalsFilePath must not be empty" }, { status: 400 });
    }
    if (config.signalArrivalPerTick < 0 || config.signalArrivalPerTick > 10) {
      return NextResponse.json({ error: "signalArrivalPerTick must be in [0, 10]" }, { status: 400 });
    }
    if (config.rejectionRate < 0 || config.rejectionRate > 1) {
      return NextResponse.json({ error: "rejectionRate must be in [0, 1]" }, { status: 400 });
    }
    if (config.startingPrice <= 0) {
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    }
    const state = startSignalBot(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
