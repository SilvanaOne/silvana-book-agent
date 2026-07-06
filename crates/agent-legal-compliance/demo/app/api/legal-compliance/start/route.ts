import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { JurisdictionRules, LegalConfig, Policy } from "@/lib/legal-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

// Policy text — two sections:
//   jurisdiction=US allowed=CC-USDC,BTC-USD prohibited= max_notional=1000000 blocks=IR,KP
//   jurisdiction=EU allowed= prohibited=SCAM-USDC blocks=IR
//   party=party::a jx=US
//   party=party::b jx=EU
function parsePolicy(raw: unknown): Policy {
  if (typeof raw !== "string") throw new Error("policy must be text");
  const partyJurisdictions: Record<string, string> = {};
  const jurisdictions: Record<string, JurisdictionRules> = {};
  const lines = raw.split("\n").map((l) => l.trim()).filter((l) => l.length > 0 && !l.startsWith("#"));
  for (const line of lines) {
    const kv = Object.fromEntries(line.split(/\s+/).map((tok) => {
      const eq = tok.indexOf("=");
      return eq >= 0 ? [tok.slice(0, eq), tok.slice(eq + 1)] : [tok, ""];
    })) as Record<string, string>;
    if (kv["jurisdiction"]) {
      const jx = kv["jurisdiction"];
      const allowed = (kv["allowed"] ?? "").split(",").map((s) => s.trim().toUpperCase()).filter((s) => s.length > 0);
      const prohibited = (kv["prohibited"] ?? "").split(",").map((s) => s.trim().toUpperCase()).filter((s) => s.length > 0);
      const blocks = (kv["blocks"] ?? "").split(",").map((s) => s.trim()).filter((s) => s.length > 0);
      const maxN = kv["max_notional"] ? num(kv["max_notional"], `${jx}.max_notional`) : undefined;
      jurisdictions[jx] = { allowedMarkets: allowed, prohibitedMarkets: prohibited, maxNotionalPerTrade: maxN, prohibitedCounterpartyJurisdictions: blocks };
    } else if (kv["party"]) {
      const party = kv["party"];
      const jx = kv["jx"];
      if (!jx) throw new Error(`party line missing jx=: ${line}`);
      partyJurisdictions[party] = jx;
    } else {
      throw new Error(`unrecognised policy line: ${line}`);
    }
  }
  return { partyJurisdictions, jurisdictions };
}

function parseList(raw: unknown, name: string): string[] {
  if (typeof raw !== "string") throw new Error(`${name} must be a comma list`);
  const list = raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0);
  if (list.length === 0) throw new Error(`${name} requires >= 1 entry`);
  return list;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const policy = parsePolicy(body.policy);
    const parties = parseList(body.parties, "parties");
    const markets = parseList(body.markets, "markets").map((m) => m.toUpperCase());
    const eventRatePerSec = num(body.eventRatePerSec, "eventRatePerSec");
    if (eventRatePerSec <= 0 || eventRatePerSec > 50) throw new Error("eventRatePerSec in (0, 50]");
    const config: LegalConfig = { policy, parties, markets, eventRatePerSec };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
