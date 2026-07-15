import { NextResponse, type NextRequest } from "next/server";
import { deleteSlot, readSession } from "@/lib/store";
import { COOKIE } from "@/lib/cookies";

export const runtime = "nodejs";

export async function DELETE(
  req: NextRequest,
  { params }: { params: { id: string } },
) {
  const token = req.cookies.get(COOKIE)?.value;
  const sess = readSession(token);
  if (!sess)
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  const ok = deleteSlot(sess.userId, params.id);
  if (!ok) return NextResponse.json({ error: "not found" }, { status: 404 });
  return NextResponse.json({ ok: true });
}
