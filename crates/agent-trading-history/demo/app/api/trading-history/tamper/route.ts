import { NextResponse } from "next/server";
import { tamperAt } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request): Promise<Response> {
  try {
    const body = (await req.json()) as { index?: unknown };
    const idxNum = typeof body.index === "number" ? body.index : Number(body.index);
    if (!Number.isInteger(idxNum) || idxNum < 0) {
      return NextResponse.json({ error: "index must be a non-negative integer" }, { status: 400 });
    }
    const result = tamperAt(idxNum);
    if (!result.ok) return NextResponse.json({ error: result.message }, { status: 400 });
    return NextResponse.json({ ok: true, message: result.message });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
