import { NextResponse } from "next/server";
import { purgeOrders } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  const res = purgeOrders();
  return NextResponse.json({ ok: true, removed: res.removed });
}
