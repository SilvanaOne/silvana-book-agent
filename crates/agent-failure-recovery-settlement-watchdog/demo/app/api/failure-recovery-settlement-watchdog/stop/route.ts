import { NextResponse } from "next/server";
import { stopFailureRecovery } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopFailureRecovery();
  return NextResponse.json({ ok: true });
}
