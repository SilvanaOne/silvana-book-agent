import { NextResponse } from "next/server";
import { stopSpreadCapture } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopSpreadCapture();
  return NextResponse.json({ ok: true });
}
