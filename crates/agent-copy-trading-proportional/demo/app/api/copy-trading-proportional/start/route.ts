import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { CopyConfig } from "@/lib/copy-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}
function optNum(v: unknown, name: string): number | undefined {
  if (v === undefined || v === null || (typeof v === "string" && v.trim().length === 0)) return undefined;
  return num(v, name);
}
function parseList(raw: unknown, upper = false): string[] {
  if (typeof raw !== "string") return [];
  return raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0).map((s) => upper ? s.toUpperCase() : s);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const leader = typeof body.leader === "string" && body.leader.trim().length > 0 ? body.leader.trim() : "party::leader";
    const follower = typeof body.follower === "string" && body.follower.trim().length > 0 ? body.follower.trim() : "party::me";
    const followerPortfolio = num(body.followerPortfolio, "followerPortfolio");
    if (followerPortfolio <= 0) throw new Error("followerPortfolio must be > 0");
    const leaderPortfolio = num(body.leaderPortfolio, "leaderPortfolio");
    if (leaderPortfolio <= 0) throw new Error("leaderPortfolio must be > 0");
    const maxScale = num(body.maxScale, "maxScale");
    if (maxScale <= 0) throw new Error("maxScale must be > 0");
    const markets = parseList(body.markets, true);
    const marketPool = parseList(body.marketPool, true);
    if (marketPool.length === 0) throw new Error("marketPool required");
    const maxLeaderNotional = optNum(body.maxLeaderNotional, "maxLeaderNotional");
    const maxMirrorNotional = optNum(body.maxMirrorNotional, "maxMirrorNotional");
    const leaderRatePerSec = num(body.leaderRatePerSec, "leaderRatePerSec");
    if (leaderRatePerSec <= 0 || leaderRatePerSec > 20) throw new Error("leaderRatePerSec in (0, 20]");
    const dryRun = Boolean(body.dryRun);

    const config: CopyConfig = {
      leader,
      follower,
      followerPortfolio,
      leaderPortfolio,
      maxScale,
      markets,
      marketPool,
      maxLeaderNotional,
      maxMirrorNotional,
      leaderRatePerSec,
      dryRun,
    };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
