import { NextResponse } from "next/server";
import { startPreTradeCheck } from "@/lib/store";
import { parseRules, type PreTradeCheckConfig } from "@/lib/pretradecheck-engine";

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
    const rulesRaw = typeof body.rulesJson === "string" ? body.rulesJson : "{}";
    const rules = parseRules(rulesRaw);
    const orderArrivalPerTick = num(body.orderArrivalPerTick, "orderArrivalPerTick");
    const startingPrice = num(body.startingPrice, "startingPrice");

    if (orderArrivalPerTick < 0 || orderArrivalPerTick > 10) {
      return NextResponse.json({ error: "orderArrivalPerTick must be in [0, 10]" }, { status: 400 });
    }
    if (startingPrice <= 0) {
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    }

    const config: PreTradeCheckConfig = { rules, orderArrivalPerTick, startingPrice };
    const state = startPreTradeCheck(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
