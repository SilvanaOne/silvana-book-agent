import { NextResponse } from "next/server";
import { rotateSignatureKey } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  const res = rotateSignatureKey();
  if (!res.ok) return NextResponse.json({ error: res.error }, { status: 409 });
  return NextResponse.json({
    publicKey: res.publicKey,
    privateKeyMasked: res.op.privateKeyMasked,
    seq: res.op.seq,
    t: res.op.t,
  });
}
