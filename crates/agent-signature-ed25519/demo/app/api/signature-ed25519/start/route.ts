import { NextResponse } from "next/server";
import { startSignature } from "@/lib/store";
import type { SignatureConfig } from "@/lib/signature-engine";

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
    const config: SignatureConfig = {
      autoDemoIntervalSecs: num(body.autoDemoIntervalSecs, "autoDemoIntervalSecs"),
      tamperRate: num(body.tamperRate, "tamperRate"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.autoDemoIntervalSecs <= 0)
      return NextResponse.json({ error: "autoDemoIntervalSecs must be > 0" }, { status: 400 });
    if (config.tamperRate < 0 || config.tamperRate > 1)
      return NextResponse.json({ error: "tamperRate must be in [0, 1]" }, { status: 400 });
    if (config.startingPrice <= 0)
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startSignature(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
