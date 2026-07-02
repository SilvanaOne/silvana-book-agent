import { NextResponse } from "next/server";
import { injectSignal } from "@/lib/store";
import type { Side } from "@/lib/signalbot-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  try {
    const body = (await req.json()) as Record<string, unknown>;
    const market = String(body.market ?? "").trim();
    const sideRaw = String(body.side ?? "").toLowerCase();
    const quantity = num(body.quantity, "quantity");
    const price = num(body.price, "price");
    const refRaw = body.ref;
    const ref = typeof refRaw === "string" && refRaw.trim() ? refRaw.trim() : undefined;

    if (!market) return NextResponse.json({ error: "market is required" }, { status: 400 });
    if (sideRaw !== "buy" && sideRaw !== "sell") {
      return NextResponse.json({ error: "side must be 'buy' or 'sell'" }, { status: 400 });
    }
    if (quantity <= 0) return NextResponse.json({ error: "quantity must be > 0" }, { status: 400 });
    if (price <= 0) return NextResponse.json({ error: "price must be > 0" }, { status: 400 });

    injectSignal({ market, side: sideRaw as Side, quantity, price, ref });
    return NextResponse.json({ ok: true });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
