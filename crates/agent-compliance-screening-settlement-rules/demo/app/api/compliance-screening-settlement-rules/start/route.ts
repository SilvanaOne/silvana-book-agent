import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { ComplianceConfig, Policy } from "@/lib/compliance-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

// Policy text:
//   blocked_pair=partyA,partyB
//   party_cap=partyX window_hours=24 max_notional=100000
//   allowed=partyA (repeat for each)
//   blocked_market=XXX-YYY (repeat)
function parsePolicy(raw: unknown): Policy {
  if (typeof raw !== "string") throw new Error("policy must be text");
  const lines = raw.split("\n").map((l) => l.trim()).filter((l) => l.length > 0 && !l.startsWith("#"));
  const blockedPairs: Array<[string, string]> = [];
  const partyCaps: Policy["partyCaps"] = [];
  const allowedCounterparties: string[] = [];
  const blockedMarkets: string[] = [];
  for (const line of lines) {
    if (line.startsWith("blocked_pair=")) {
      const pair = line.slice("blocked_pair=".length).split(",").map((s) => s.trim());
      if (pair.length !== 2 || !pair[0] || !pair[1]) throw new Error(`bad blocked_pair: ${line}`);
      blockedPairs.push([pair[0], pair[1]]);
    } else if (line.startsWith("party_cap=")) {
      const rest = line.slice("party_cap=".length);
      const tokens = rest.split(/\s+/);
      const party = tokens[0];
      const kv = Object.fromEntries(tokens.slice(1).map((t) => t.split("="))) as Record<string, string>;
      const windowHours = Math.max(1, Math.floor(num(kv["window_hours"] ?? "24", "window_hours")));
      const maxNotional = num(kv["max_notional"] ?? "0", "max_notional");
      if (!party || maxNotional <= 0) throw new Error(`bad party_cap: ${line}`);
      partyCaps.push({ party, windowHours, maxNotional });
    } else if (line.startsWith("allowed=")) {
      allowedCounterparties.push(line.slice("allowed=".length).trim());
    } else if (line.startsWith("blocked_market=")) {
      blockedMarkets.push(line.slice("blocked_market=".length).trim().toUpperCase());
    } else {
      throw new Error(`unrecognised policy line: ${line}`);
    }
  }
  return { blockedPairs, partyCaps, allowedCounterparties, blockedMarkets };
}

function parseList(raw: unknown, name: string): string[] {
  if (typeof raw !== "string") throw new Error(`${name} must be a comma list`);
  const list = raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0);
  if (list.length === 0) throw new Error(`${name} requires at least one entry`);
  return list;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const policy = parsePolicy(body.policy);
    const emitAccepts = Boolean(body.emitAccepts);
    const eventRatePerSec = num(body.eventRatePerSec, "eventRatePerSec");
    if (eventRatePerSec <= 0 || eventRatePerSec > 50) return NextResponse.json({ error: "eventRatePerSec must be in (0, 50]" }, { status: 400 });
    const parties = parseList(body.parties, "parties");
    const markets = parseList(body.markets, "markets").map((m) => m.toUpperCase());
    for (const m of markets) if (!/^[A-Z0-9]+-[A-Z0-9]+$/.test(m)) throw new Error(`market "${m}" must look like BASE-QUOTE`);
    const marketFilter = typeof body.marketFilter === "string" && body.marketFilter.trim().length > 0 ? body.marketFilter.trim().toUpperCase() : undefined;

    const config: ComplianceConfig = { policy, emitAccepts, eventRatePerSec, parties, markets, market: marketFilter };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
