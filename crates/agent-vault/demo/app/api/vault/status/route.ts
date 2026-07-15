import { NextResponse, type NextRequest } from "next/server";
import { findUser, readSession } from "@/lib/store";
import { COOKIE } from "@/lib/cookies";

export const runtime = "nodejs";

export async function GET(req: NextRequest) {
  const token = req.cookies.get(COOKIE)?.value;
  const sess = readSession(token);
  if (!sess)
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  const user = findUser(sess.userId);
  return NextResponse.json({ slots: user?.slots ?? [] });
}
