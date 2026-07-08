import { NextResponse } from "next/server";
import { stopReadinessCheck } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopReadinessCheck();
  return NextResponse.json({ ok: true });
}
