import { NextResponse, type NextRequest } from "next/server";
import { addWalletSlot, readSession } from "@/lib/store";
import { COOKIE } from "@/lib/cookies";

export const runtime = "nodejs";

export async function POST(req: NextRequest) {
  const token = req.cookies.get(COOKIE)?.value;
  const sess = readSession(token);
  if (!sess)
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  const body = (await req.json().catch(() => ({}))) as {
    label?: string;
    partyId?: string;
    keyId?: string;
    privKeyCiphertext?: string;
    seedCiphertext?: string;
  };
  if (!body.label) {
    return NextResponse.json(
      { error: "label is required" },
      { status: 400 },
    );
  }
  if (!body.privKeyCiphertext && !body.seedCiphertext) {
    return NextResponse.json(
      { error: "ciphertext is required" },
      { status: 400 },
    );
  }
  const slot = addWalletSlot(sess.userId, {
    label: body.label,
    partyId: body.partyId,
  });
  if (!slot)
    return NextResponse.json({ error: "user not found" }, { status: 404 });
  return NextResponse.json({ id: slot.id });
}
