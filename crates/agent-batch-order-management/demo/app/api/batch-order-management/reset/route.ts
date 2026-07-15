import { NextResponse } from "next/server";
import { resetPortfolio } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  resetPortfolio();
  return NextResponse.json({ ok: true });
}
