import { NextResponse, type NextRequest } from "next/server";
import { loginOrCreate, newSession } from "@/lib/store";
import { setSessionCookieHeader } from "@/lib/cookies";

export const runtime = "nodejs";

export async function POST(req: NextRequest) {
  const body = (await req.json().catch(() => ({}))) as {
    emailOrUsername?: string;
    username?: string;
    password?: string;
  };
  const login = body.emailOrUsername || body.username || "";
  if (!login || !body.password) {
    return NextResponse.json(
      { ok: false, error: "Invalid login or password" },
      { status: 401 },
    );
  }
  const user = loginOrCreate(login);
  const sess = newSession(user.id);
  const res = NextResponse.json({
    ok: true,
    kind: "tenant",
    userId: user.id,
    username: user.username,
    email: user.email,
  });
  res.headers.set("Set-Cookie", setSessionCookieHeader(sess.token));
  return res;
}
