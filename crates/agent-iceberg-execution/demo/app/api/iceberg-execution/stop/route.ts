import { NextResponse } from "next/server";
import { stopIcebergExecution } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopIcebergExecution();
  return NextResponse.json({ ok: true });
}
