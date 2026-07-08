import { NextResponse } from "next/server";
import { stopInfiniteGrid } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopInfiniteGrid();
  return NextResponse.json({ ok: true });
}
