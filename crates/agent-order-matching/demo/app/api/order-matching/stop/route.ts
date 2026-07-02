import { NextResponse } from "next/server";
import { stopOrderMatching } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopOrderMatching();
  return NextResponse.json({ ok: true });
}
