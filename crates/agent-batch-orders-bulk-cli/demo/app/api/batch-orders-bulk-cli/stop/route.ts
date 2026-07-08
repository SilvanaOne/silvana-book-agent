import { NextResponse } from "next/server";
import { stopBatchOrders } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopBatchOrders();
  return NextResponse.json({ ok: true });
}
