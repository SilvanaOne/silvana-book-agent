import { NextResponse } from "next/server";
import { startBlockedParty } from "@/lib/store";
import type { BlockedPartyConfig } from "@/lib/blockedparty-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function normalizeBlocklist(input: unknown): string[] {
  if (Array.isArray(input)) {
    return input.map((x) => String(x));
  }
  if (typeof input === "string") {
    return input.split(/\r?\n/);
  }
  return [];
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const cleaned = normalizeBlocklist(body.blocklist)
      .map((s) => s.trim())
      .filter((s) => s.length > 0 && !s.startsWith("#"));
    const config: BlockedPartyConfig = {
      blocklist: cleaned,
      reloadSecs: num(body.reloadSecs, "reloadSecs"),
      settlementArrivalPerTick: num(body.settlementArrivalPerTick, "settlementArrivalPerTick"),
      blockedPartyProbability: num(body.blockedPartyProbability, "blockedPartyProbability"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.blocklist.length === 0) return NextResponse.json({ error: "blocklist must contain at least one entry" }, { status: 400 });
    if (config.reloadSecs <= 0) return NextResponse.json({ error: "reloadSecs must be > 0" }, { status: 400 });
    if (config.settlementArrivalPerTick < 0 || config.settlementArrivalPerTick > 10) return NextResponse.json({ error: "settlementArrivalPerTick must be in [0, 10]" }, { status: 400 });
    if (config.blockedPartyProbability < 0 || config.blockedPartyProbability > 1) return NextResponse.json({ error: "blockedPartyProbability must be in [0, 1]" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startBlockedParty(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
