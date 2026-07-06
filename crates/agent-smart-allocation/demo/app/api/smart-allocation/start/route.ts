import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { Bucket, InstrumentSlot, SmartAllocConfig } from "@/lib/smartalloc-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

// Policy text format (one bucket per group of lines, buckets separated by blank line):
//   bucket=<name> weight=<w>
//   instrument=<name> market=<BASE-QUOTE> weight=<w> balance=<n> price=<n>
//   ...
//
// Example:
//   bucket=stables weight=0.5
//   instrument=Amulet market=CC-USDC weight=1.0 balance=500 price=1.0
//
//   bucket=btc-theme weight=0.5
//   instrument=CBTC market=CBTC-CC weight=1.0 balance=0.008 price=60000
function parsePolicy(raw: unknown): { buckets: Bucket[]; balances: Record<string, number>; prices: Record<string, number> } {
  if (typeof raw !== "string") throw new Error("policy must be a text block");
  const lines = raw.split("\n").map((l) => l.trim());
  const buckets: Bucket[] = [];
  const balances: Record<string, number> = {};
  const prices: Record<string, number> = {};

  let curName = "";
  let curWeight = 0;
  let curInstruments: InstrumentSlot[] = [];

  const push = () => {
    if (curName) {
      buckets.push({ name: curName, weight: curWeight, instruments: curInstruments });
      curName = "";
      curWeight = 0;
      curInstruments = [];
    }
  };

  for (const line of lines) {
    if (line.length === 0) { push(); continue; }
    if (line.startsWith("#")) continue;
    const fields = Object.fromEntries(
      line.split(/\s+/).map((tok) => {
        const [k, v] = tok.split("=");
        return [k.trim(), (v ?? "").trim()];
      })
    ) as Record<string, string>;
    if ("bucket" in fields) {
      push();
      curName = fields.bucket;
      curWeight = Number(fields.weight ?? "0");
      if (!curName || !Number.isFinite(curWeight) || curWeight < 0) throw new Error(`bad bucket line: ${line}`);
    } else if ("instrument" in fields) {
      if (!curName) throw new Error(`instrument line without bucket: ${line}`);
      const instrument = fields.instrument;
      const market = fields.market;
      const weight = Number(fields.weight ?? "0");
      if (!instrument || !market || !Number.isFinite(weight)) throw new Error(`bad instrument line: ${line}`);
      if (!/^[A-Z0-9]+-[A-Z0-9]+$/.test(market)) throw new Error(`market "${market}" must look like BASE-QUOTE`);
      curInstruments.push({ instrument, market, weight });
      if ("balance" in fields) {
        const b = Number(fields.balance);
        if (Number.isFinite(b) && b >= 0) balances[instrument] = b;
      }
      if ("price" in fields) {
        const p = Number(fields.price);
        if (Number.isFinite(p) && p > 0) prices[market] = p;
      }
    } else {
      throw new Error(`unrecognised policy line: ${line}`);
    }
  }
  push();

  if (buckets.length === 0) throw new Error("policy has zero buckets");

  // Normalise bucket weights (rust source does this too).
  const bSum = buckets.reduce((a, b) => a + b.weight, 0);
  if (bSum <= 0) throw new Error("bucket weights sum to 0");
  const normalised: Bucket[] = buckets.map((b) => {
    const iSum = b.instruments.reduce((a, i) => a + i.weight, 0);
    return {
      name: b.name,
      weight: b.weight / bSum,
      instruments: b.instruments.map((i) => ({
        instrument: i.instrument,
        market: i.market,
        weight: iSum > 0 ? i.weight / iSum : 0,
      })),
    };
  });

  return { buckets: normalised, balances, prices };
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const { buckets, balances, prices } = parsePolicy(body.policy);
    const bucketThresholdPct = num(body.bucketThresholdPct, "bucketThresholdPct");
    if (bucketThresholdPct < 0) return NextResponse.json({ error: "bucketThresholdPct must be >= 0" }, { status: 400 });
    const rebalanceFraction = num(body.rebalanceFraction, "rebalanceFraction");
    if (rebalanceFraction <= 0 || rebalanceFraction > 1) return NextResponse.json({ error: "rebalanceFraction must be in (0, 1]" }, { status: 400 });
    const checkIntervalSecs = Math.max(1, Math.min(300, Math.floor(num(body.checkIntervalSecs, "checkIntervalSecs"))));
    const balanceDriftPerCycle = num(body.balanceDriftPerCycle, "balanceDriftPerCycle");

    const config: SmartAllocConfig = {
      buckets,
      bucketThresholdPct,
      rebalanceFraction,
      checkIntervalSecs,
      startingBalances: balances,
      startingPrices: prices,
      balanceDriftPerCycle,
    };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
