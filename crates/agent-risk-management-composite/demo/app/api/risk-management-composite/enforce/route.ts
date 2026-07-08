import { NextResponse } from "next/server";
import { toggleEnforce } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  const enforce = toggleEnforce();
  return NextResponse.json({ enforce });
}
