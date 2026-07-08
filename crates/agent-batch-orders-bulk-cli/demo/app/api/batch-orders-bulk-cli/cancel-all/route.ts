import { NextResponse } from "next/server";
import { cancelAllBatchOrders } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  cancelAllBatchOrders();
  return NextResponse.json({ ok: true });
}
