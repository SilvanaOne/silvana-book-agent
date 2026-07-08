import { NextResponse } from "next/server";
import { stopPnlScreening } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopPnlScreening();
  return NextResponse.json({ ok: true });
}
