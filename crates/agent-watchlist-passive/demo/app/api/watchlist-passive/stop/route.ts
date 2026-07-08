import { NextResponse } from "next/server";
import { stopWatchlist } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopWatchlist();
  return NextResponse.json({ ok: true });
}
