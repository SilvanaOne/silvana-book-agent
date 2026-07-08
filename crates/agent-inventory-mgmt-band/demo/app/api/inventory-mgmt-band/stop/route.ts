import { NextResponse } from "next/server";
import { stopInventoryMgmt } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopInventoryMgmt();
  return NextResponse.json({ ok: true });
}
