import { NextResponse } from "next/server";
import { stopBlockExecution } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopBlockExecution();
  return NextResponse.json({ ok: true });
}
