import { NextResponse } from "next/server";
import { stopAuth } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopAuth();
  return NextResponse.json({ ok: true });
}
