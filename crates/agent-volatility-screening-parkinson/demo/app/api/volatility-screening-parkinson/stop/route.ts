import { NextResponse } from "next/server";
import { stopVolatilityScreening } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopVolatilityScreening();
  return NextResponse.json({ ok: true });
}
