import { NextResponse } from "next/server";
import { stopPortfolioHealth } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopPortfolioHealth();
  return NextResponse.json({ ok: true });
}
