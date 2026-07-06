import { NextResponse } from "next/server";
import { triggerRun } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  triggerRun();
  return NextResponse.json({ ok: true });
}
