import { NextResponse } from "next/server";
import { stopSignature } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopSignature();
  return NextResponse.json({ ok: true });
}
