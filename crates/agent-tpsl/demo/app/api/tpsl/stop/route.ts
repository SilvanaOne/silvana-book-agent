import { NextResponse } from "next/server";
import { stopPosition } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopPosition();
  return NextResponse.json({ ok: true });
}
