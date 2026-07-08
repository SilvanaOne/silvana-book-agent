import { NextResponse } from "next/server";
import { stopOrderExpiry } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopOrderExpiry();
  return NextResponse.json({ ok: true });
}
