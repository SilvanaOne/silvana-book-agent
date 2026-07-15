import { NextResponse, type NextRequest } from "next/server";
import { findUser, readSession } from "@/lib/store";
import { COOKIE } from "@/lib/cookies";

export const runtime = "nodejs";

export async function GET(req: NextRequest) {
  const token = req.cookies.get(COOKIE)?.value;
  const sess = readSession(token);
  if (!sess) return NextResponse.json({ kind: "anonymous" });
  const user = findUser(sess.userId);
  if (!user) return NextResponse.json({ kind: "anonymous" });
  return NextResponse.json({
    kind: "tenant",
    userId: user.id,
    username: user.username,
    email: user.email,
  });
}
