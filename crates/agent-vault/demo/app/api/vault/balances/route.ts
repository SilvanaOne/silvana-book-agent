import { NextResponse, type NextRequest } from "next/server";
import { balancesFor, readSession } from "@/lib/store";
import { COOKIE } from "@/lib/cookies";

export const runtime = "nodejs";

export async function GET(req: NextRequest) {
  const token = req.cookies.get(COOKIE)?.value;
  const sess = readSession(token);
  if (!sess)
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  return NextResponse.json(balancesFor(sess.userId));
}
