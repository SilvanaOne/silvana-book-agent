import { NextResponse } from "next/server";
import { stopOracle } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopOracle();
  return NextResponse.json({ ok: true });
}
