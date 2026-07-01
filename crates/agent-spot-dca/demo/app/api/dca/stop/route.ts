import { NextResponse } from "next/server";
import { stopDca } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopDca();
  return NextResponse.json({ ok: true });
}
