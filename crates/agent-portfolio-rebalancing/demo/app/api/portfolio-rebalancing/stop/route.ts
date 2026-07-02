import { NextResponse } from "next/server";
import { stopPortfolioRebalancing } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopPortfolioRebalancing();
  return NextResponse.json({ ok: true });
}
