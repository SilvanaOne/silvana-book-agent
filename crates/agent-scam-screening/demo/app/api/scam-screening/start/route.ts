import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { Category, ScamConfig, Severity } from "@/lib/scam-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

// Categories text — one category per line:
//   category=scam_db severity=warn parties=party::a,party::b
function parseCategories(raw: unknown): Category[] {
  if (typeof raw !== "string") throw new Error("categories must be text");
  const lines = raw.split("\n").map((l) => l.trim()).filter((l) => l.length > 0 && !l.startsWith("#"));
  return lines.map((line, i) => {
    const kv = Object.fromEntries(line.split(/\s+/).map((tok) => {
      const eq = tok.indexOf("=");
      return eq >= 0 ? [tok.slice(0, eq), tok.slice(eq + 1)] : [tok, ""];
    })) as Record<string, string>;
    if (!kv.category || !kv.severity) throw new Error(`category ${i + 1}: missing category= or severity=`);
    const severity = kv.severity as Severity;
    if (!["info", "warn", "critical"].includes(severity)) throw new Error(`category ${i + 1}: bad severity`);
    const parties = (kv.parties ?? "").split(",").map((s) => s.trim()).filter((s) => s.length > 0);
    return { name: kv.category, severity, parties };
  });
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
    const categories = parseCategories(body.categories);
    if (categories.length === 0) throw new Error("no categories defined");
    const parties = parseList(body.parties, "parties");
    const markets = parseList(body.markets, "markets").map((m) => m.toUpperCase());
    const eventRatePerSec = num(body.eventRatePerSec, "eventRatePerSec");
    if (eventRatePerSec <= 0 || eventRatePerSec > 50) throw new Error("eventRatePerSec in (0, 50]");
    const refreshSecs = Math.max(1, Math.min(3600, Math.floor(num(body.refreshSecs, "refreshSecs"))));

    const config: ScamConfig = { categories, parties, markets, eventRatePerSec, refreshSecs };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
