import { NextResponse } from "next/server";
import { stopTrendFollowing } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopTrendFollowing();
  return NextResponse.json({ ok: true });
}
