import { NextResponse } from "next/server";
import { stopCashBuffer } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopCashBuffer();
  return NextResponse.json({ ok: true });
}
