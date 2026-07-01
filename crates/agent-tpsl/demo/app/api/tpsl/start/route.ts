import { NextResponse } from "next/server";
import { startPosition } from "@/lib/store";
import type { PositionConfig, Side } from "@/lib/tpsl-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function parseNumber(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number, got ${JSON.stringify(v)}`);
  return n;
}

function parseOptionalNumber(v: unknown): number | null {
  if (v === null || v === undefined || v === "") return null;
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? n : null;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const side = String(body.side ?? "long").toLowerCase();
    if (side !== "long" && side !== "short") {
      return NextResponse.json({ error: "side must be long or short" }, { status: 400 });
    }
    const config: PositionConfig = {
      market: String(body.market ?? "CC-USDC"),
      side: side as Side,
      quantity: parseNumber(body.quantity, "quantity"),
      entryPrice: parseNumber(body.entryPrice, "entryPrice"),
      tp: parseOptionalNumber(body.tp),
      sl: parseOptionalNumber(body.sl),
      trailingPct: parseOptionalNumber(body.trailingPct),
    };
    if (config.quantity <= 0) {
      return NextResponse.json({ error: "quantity must be > 0" }, { status: 400 });
    }
    if (config.entryPrice <= 0) {
      return NextResponse.json({ error: "entryPrice must be > 0" }, { status: 400 });
    }
    const state = startPosition(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
