import { NextResponse, type NextRequest } from "next/server";
import { newSession, upsertUser } from "@/lib/store";
import { setSessionCookieHeader } from "@/lib/cookies";

export const runtime = "nodejs";

export async function POST(req: NextRequest) {
  const body = (await req.json().catch(() => ({}))) as {
    username?: string;
    email?: string;
    password?: string;
  };
  const username = (body.username || "").trim();
  const email = (body.email || "").trim();
  if (username.length < 3) {
    return NextResponse.json(
      { ok: false, error: "Username must be at least 3 characters" },
      { status: 400 },
    );
  }
  if (!email.includes("@")) {
    return NextResponse.json(
      { ok: false, error: "Enter a valid email" },
      { status: 400 },
    );
  }
  if (!body.password || body.password.length < 8) {
    return NextResponse.json(
      { ok: false, error: "Password must be at least 8 characters" },
      { status: 400 },
    );
  }
  const user = upsertUser({ username, email });
  const sess = newSession(user.id);
  const res = NextResponse.json({
    ok: true,
    sessionId: sess.token,
    userId: user.id,
    username: user.username,
    email: user.email,
  });
  res.headers.set("Set-Cookie", setSessionCookieHeader(sess.token));
  return res;
}
