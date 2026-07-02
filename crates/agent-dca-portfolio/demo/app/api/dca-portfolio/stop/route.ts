import { NextResponse } from "next/server";
import { stopDcaPortfolio } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopDcaPortfolio();
  return NextResponse.json({ ok: true });
}
