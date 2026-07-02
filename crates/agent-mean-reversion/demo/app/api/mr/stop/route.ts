import { NextResponse } from "next/server";
import { stopMr } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopMr();
  return NextResponse.json({ ok: true });
}
