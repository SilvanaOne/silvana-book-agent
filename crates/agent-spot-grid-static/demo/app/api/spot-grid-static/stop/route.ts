import { NextResponse } from "next/server";
import { stopSpotGrid } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopSpotGrid();
  return NextResponse.json({ ok: true });
}
