import { NextResponse } from "next/server";
import { startDca } from "@/lib/store";
import type { DcaConfig, Side } from "@/lib/dca-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}
function optNum(v: unknown): number | null {
  if (v === null || v === undefined || v === "") return null;
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? n : null;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const side = String(body.side ?? "buy").toLowerCase();
    if (side !== "buy" && side !== "sell") return NextResponse.json({ error: "side must be buy or sell" }, { status: 400 });
    const config: DcaConfig = {
      market: String(body.market ?? "CC-USDC"),
      side: side as Side,
      amountPerOrder: num(body.amountPerOrder, "amountPerOrder"),
      intervalSecs: num(body.intervalSecs, "intervalSecs"),
      priceOffsetPct: num(body.priceOffsetPct, "priceOffsetPct"),
      maxTotal: optNum(body.maxTotal),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.amountPerOrder <= 0) return NextResponse.json({ error: "amountPerOrder must be > 0" }, { status: 400 });
    if (config.intervalSecs <= 0) return NextResponse.json({ error: "intervalSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startDca(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
