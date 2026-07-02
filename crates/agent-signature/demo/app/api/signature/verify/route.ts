import { NextResponse } from "next/server";
import { verifyMessage } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request): Promise<Response> {
  let body: Record<string, unknown> = {};
  try {
    body = (await req.json()) as Record<string, unknown>;
  } catch {
    return NextResponse.json({ error: "invalid JSON body" }, { status: 400 });
  }
  const message = typeof body.message === "string" ? body.message : "";
  const signature = typeof body.signature === "string" ? body.signature : "";
  if (!message) return NextResponse.json({ error: "message must be a non-empty string" }, { status: 400 });
  if (!signature) return NextResponse.json({ error: "signature must be a non-empty string" }, { status: 400 });

  const res = verifyMessage(message, signature);
  if (!res.ok) return NextResponse.json({ error: res.error }, { status: 409 });
  return NextResponse.json({
    ok: res.op.verified === true,
    seq: res.op.seq,
    t: res.op.t,
  });
}
