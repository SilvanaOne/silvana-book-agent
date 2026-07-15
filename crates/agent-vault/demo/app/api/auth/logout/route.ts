import { NextResponse, type NextRequest } from "next/server";
import { dropSession } from "@/lib/store";
import { COOKIE, clearSessionCookieHeader } from "@/lib/cookies";

export const runtime = "nodejs";

export async function POST(req: NextRequest) {
  const token = req.cookies.get(COOKIE)?.value;
  dropSession(token);
  const res = NextResponse.json({ ok: true });
  res.headers.set("Set-Cookie", clearSessionCookieHeader());
  return res;
}
