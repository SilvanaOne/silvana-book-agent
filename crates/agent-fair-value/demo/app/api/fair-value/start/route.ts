import { NextResponse } from "next/server";
import { startFairValue } from "@/lib/store";
import type { AggMethod, FairValueConfig } from "@/lib/fairvalue-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

const METHODS: readonly AggMethod[] = ["median", "mean", "trimmed-mean"];

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const method = String(body.method ?? "median") as AggMethod;
    if (!METHODS.includes(method)) {
      return NextResponse.json({ error: `method must be one of ${METHODS.join(", ")}` }, { status: 400 });
    }
    const sourcesRaw = body.sources;
    let sources: string[] = [];
    if (typeof sourcesRaw === "string") {
      sources = sourcesRaw.split(",").map((s) => s.trim()).filter((s) => s.length > 0);
    } else if (Array.isArray(sourcesRaw)) {
      sources = sourcesRaw.map((s) => String(s).trim()).filter((s) => s.length > 0);
    }
    if (sources.length === 0) {
      return NextResponse.json({ error: "sources must be a non-empty list" }, { status: 400 });
    }
    // Deduplicate but preserve order.
    const seen = new Set<string>();
    sources = sources.filter((s) => (seen.has(s) ? false : (seen.add(s), true)));

    const config: FairValueConfig = {
      market: String(body.market ?? "CC-USDC"),
      sources,
      method,
      pollSecs: num(body.pollSecs, "pollSecs"),
      startingPrice: num(body.startingPrice, "startingPrice"),
      sourceNoisePct: num(body.sourceNoisePct, "sourceNoisePct"),
    };
    if (config.pollSecs <= 0) return NextResponse.json({ error: "pollSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (config.sourceNoisePct < 0) return NextResponse.json({ error: "sourceNoisePct must be >= 0" }, { status: 400 });
    const state = startFairValue(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
