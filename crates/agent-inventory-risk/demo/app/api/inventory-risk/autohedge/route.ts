import { NextResponse } from "next/server";
import { toggleAutoHedge } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  const autoHedge = toggleAutoHedge();
  return NextResponse.json({ autoHedge });
}
