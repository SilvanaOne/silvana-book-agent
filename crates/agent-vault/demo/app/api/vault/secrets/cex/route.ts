import { NextResponse, type NextRequest } from "next/server";
import { addCexSlot, readSession, type CexVenue } from "@/lib/store";
import { COOKIE } from "@/lib/cookies";

export const runtime = "nodejs";

export async function POST(req: NextRequest) {
  const token = req.cookies.get(COOKIE)?.value;
  const sess = readSession(token);
  if (!sess)
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  const body = (await req.json().catch(() => ({}))) as {
    venue?: string;
    label?: string;
    apiKeyCiphertext?: string;
    secretCiphertext?: string;
    passphraseCiphertext?: string;
  };
  if (body.venue !== "bybit" && body.venue !== "kucoin") {
    return NextResponse.json(
      { error: "venue must be bybit or kucoin" },
      { status: 400 },
    );
  }
  if (!body.apiKeyCiphertext || !body.secretCiphertext) {
    return NextResponse.json(
      { error: "api key and secret ciphertext required" },
      { status: 400 },
    );
  }
  const slot = addCexSlot(sess.userId, {
    venue: body.venue as CexVenue,
    label: body.label,
  });
  if (!slot)
    return NextResponse.json({ error: "user not found" }, { status: 404 });
  return NextResponse.json({ id: slot.id });
}
