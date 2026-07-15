import { NextResponse, type NextRequest } from "next/server";
import { findUser, newSession, readSession, upsertUser } from "@/lib/store";
import { COOKIE, setSessionCookieHeader } from "@/lib/cookies";

export const runtime = "nodejs";

// No login in the demo: the first call auto-provisions a per-visitor tenant
// (user + session cookie); subsequent calls return the same tenant.
export async function GET(req: NextRequest) {
  const token = req.cookies.get(COOKIE)?.value;
  const sess = readSession(token);
  const existing = sess ? findUser(sess.userId) : null;
  if (existing) {
    return NextResponse.json({
      kind: "tenant",
      userId: existing.id,
      username: existing.username,
      email: existing.email,
    });
  }
  const user = upsertUser({});
  const fresh = newSession(user.id);
  const res = NextResponse.json({
    kind: "tenant",
    userId: user.id,
    username: user.username,
    email: user.email,
  });
  res.headers.set("Set-Cookie", setSessionCookieHeader(fresh.token));
  return res;
}
