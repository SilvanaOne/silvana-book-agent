import { NextResponse } from "next/server";
import { signMessage } from "@/lib/store";

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
  const canonical = body.canonical === true;
  if (!message) return NextResponse.json({ error: "message must be a non-empty string" }, { status: 400 });
  if (message.length > 2048)
    return NextResponse.json({ error: "message too long (max 2048 chars)" }, { status: 400 });

  const res = signMessage(message, canonical);
  if (!res.ok) return NextResponse.json({ error: res.error }, { status: 409 });
  return NextResponse.json({
    signature: res.op.output,
    publicKey: res.publicKey,
    kind: res.op.kind,
    seq: res.op.seq,
    t: res.op.t,
  });
}
