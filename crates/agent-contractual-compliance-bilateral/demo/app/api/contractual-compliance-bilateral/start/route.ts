import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { Contract, ContractConfig } from "@/lib/contract-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

// Contracts text (one per line):
//   id=lp-cc-usdc counterparty=party::x market=CC-USDC window_hours=24 min=1000 max=100000 expires=2027-01-01
function parseContracts(raw: unknown): Contract[] {
  if (typeof raw !== "string") throw new Error("contracts must be text");
  const lines = raw.split("\n").map((l) => l.trim()).filter((l) => l.length > 0 && !l.startsWith("#"));
  return lines.map((line, i) => {
    const kv = Object.fromEntries(line.split(/\s+/).map((tok) => tok.split("="))) as Record<string, string>;
    if (!kv.id || !kv.counterparty || !kv.market || !kv.window_hours) throw new Error(`contract ${i + 1}: missing id/counterparty/market/window_hours`);
    if (!/^[A-Z0-9]+-[A-Z0-9]+$/i.test(kv.market)) throw new Error(`contract ${i + 1}: market must look like BASE-QUOTE`);
    const c: Contract = {
      id: kv.id,
      counterparty: kv.counterparty,
      market: kv.market.toUpperCase(),
      windowHours: Math.max(1, Math.floor(num(kv.window_hours, `contract ${i + 1}.window_hours`))),
      minNotional: kv.min ? num(kv.min, `contract ${i + 1}.min`) : undefined,
      maxNotional: kv.max ? num(kv.max, `contract ${i + 1}.max`) : undefined,
      expiresAtISO: kv.expires ?? undefined,
    };
    return c;
  });
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
    const contracts = parseContracts(body.contracts);
    if (contracts.length === 0) throw new Error("no contracts defined");
    const parties = parseList(body.parties, "parties");
    const markets = parseList(body.markets, "markets").map((m) => m.toUpperCase());
    const eventRatePerSec = num(body.eventRatePerSec, "eventRatePerSec");
    if (eventRatePerSec <= 0 || eventRatePerSec > 50) throw new Error("eventRatePerSec in (0, 50]");
    const statusIntervalSecs = Math.max(1, Math.min(600, Math.floor(num(body.statusIntervalSecs, "statusIntervalSecs"))));

    const config: ContractConfig = { contracts, parties, markets, eventRatePerSec, statusIntervalSecs };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
