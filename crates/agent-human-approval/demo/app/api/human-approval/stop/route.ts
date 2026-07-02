import { NextResponse } from "next/server";
import { stopHumanApproval } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopHumanApproval();
  return NextResponse.json({ ok: true });
}
