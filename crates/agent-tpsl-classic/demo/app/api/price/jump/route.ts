import { NextResponse } from "next/server";
import { jumpPrice } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  const to = Number(body.to);
  if (!Number.isFinite(to) || to <= 0) {
    return NextResponse.json({ error: "to must be a positive number" }, { status: 400 });
  }
  jumpPrice(to);
  return NextResponse.json({ ok: true });
}
