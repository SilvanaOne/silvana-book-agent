import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { RetentionConfig } from "@/lib/retention-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const totalDays = Math.max(2, Math.min(60, Math.floor(num(body.totalDays, "totalDays"))));
    const recordsPerDay = Math.max(5, Math.min(200, Math.floor(num(body.recordsPerDay, "recordsPerDay"))));
    const weekly = Boolean(body.weekly);
    const retentionRaw = body.retentionDays;
    let retentionDays: number | undefined;
    if (retentionRaw !== undefined && retentionRaw !== null && !(typeof retentionRaw === "string" && retentionRaw.trim().length === 0)) {
      const rd = num(retentionRaw, "retentionDays");
      if (rd < 0) throw new Error("retentionDays must be >= 0");
      retentionDays = Math.floor(rd);
    }
    const party = typeof body.party === "string" && body.party.trim().length > 0 ? body.party.trim() : "party::demo";
    const config: RetentionConfig = { totalDays, recordsPerDay, retentionDays, weekly, party };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
