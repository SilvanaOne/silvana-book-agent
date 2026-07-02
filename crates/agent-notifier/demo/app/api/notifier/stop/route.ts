import { NextResponse } from "next/server";
import { stopNotifier } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopNotifier();
  return NextResponse.json({ ok: true });
}
