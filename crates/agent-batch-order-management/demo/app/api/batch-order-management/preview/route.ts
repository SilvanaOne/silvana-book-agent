import { NextResponse } from "next/server";
import { previewRebalance } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  const plan = previewRebalance();
  return NextResponse.json({ plan });
}
