import { NextResponse } from "next/server";
import { startWitnesses } from "@/lib/store";
import { parseHandlers, type WitnessesConfig } from "@/lib/witnesses-engine";

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
    const handlersText = String(body.handlersText ?? "").trim();
    if (!handlersText) return NextResponse.json({ error: "handlers must be non-empty" }, { status: 400 });
    const handlers = parseHandlers(handlersText);

    const config: WitnessesConfig = {
      handlers,
      market: String(body.market ?? "").trim(),
      eventArrivalPerTick: num(body.eventArrivalPerTick, "eventArrivalPerTick"),
      commandDurationMs: num(body.commandDurationMs, "commandDurationMs"),
      commandFailureRate: num(body.commandFailureRate, "commandFailureRate"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.eventArrivalPerTick < 0 || config.eventArrivalPerTick > 1) {
      return NextResponse.json({ error: "eventArrivalPerTick must be in [0, 1]" }, { status: 400 });
    }
    if (config.commandDurationMs < 0) {
      return NextResponse.json({ error: "commandDurationMs must be >= 0" }, { status: 400 });
    }
    if (config.commandFailureRate < 0 || config.commandFailureRate > 1) {
      return NextResponse.json({ error: "commandFailureRate must be in [0, 1]" }, { status: 400 });
    }
    if (config.startingPrice <= 0) {
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    }

    const state = startWitnesses(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
