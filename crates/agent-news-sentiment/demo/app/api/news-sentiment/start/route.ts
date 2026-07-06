import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { NewsConfig } from "@/lib/news-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}
function parseLines(raw: unknown): string[] {
  if (typeof raw !== "string") return [];
  return raw.split("\n").map((s) => s.trim()).filter((s) => s.length > 0 && !s.startsWith("#"));
}
function parseAliases(raw: unknown): Record<string, string[]> {
  if (typeof raw !== "string") throw new Error("aliases required");
  const out: Record<string, string[]> = {};
  for (const line of raw.split("\n").map((s) => s.trim()).filter((s) => s.length > 0 && !s.startsWith("#"))) {
    const eq = line.indexOf("=");
    if (eq < 0) throw new Error(`bad alias line "${line}" — expected MARKET=alias1,alias2`);
    const market = line.slice(0, eq).trim().toUpperCase();
    const list = line.slice(eq + 1).split(",").map((s) => s.trim().toLowerCase()).filter((s) => s.length > 0);
    if (!market || list.length === 0) throw new Error(`bad alias line "${line}"`);
    out[market] = list;
  }
  return out;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const threshold = num(body.threshold, "threshold");
    if (threshold < 0 || threshold > 1) throw new Error("threshold in [0, 1]");
    const quantity = num(body.quantity, "quantity");
    if (quantity <= 0) throw new Error("quantity > 0");
    const aliases = parseAliases(body.aliases);
    if (Object.keys(aliases).length === 0) throw new Error("at least one market alias required");
    const headlinesPerSec = num(body.headlinesPerSec, "headlinesPerSec");
    if (headlinesPerSec <= 0 || headlinesPerSec > 10) throw new Error("headlinesPerSec in (0, 10]");
    const headlinePool = parseLines(body.headlinePool);
    if (headlinePool.length === 0) throw new Error("headlinePool required");

    const config: NewsConfig = { threshold, quantity, aliases, headlinesPerSec, headlinePool };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
