import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { AiSignalConfig } from "@/lib/ai-signal-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}
function parseList(raw: unknown, upper = false): string[] {
  if (typeof raw !== "string") return [];
  return raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0).map((s) => upper ? s.toUpperCase() : s);
}
function parseMids(raw: unknown, expected: number): number[] {
  const parts = parseList(raw);
  if (parts.length !== expected) throw new Error(`mids must have ${expected} entries (one per market)`);
  return parts.map((p, i) => { const n = Number(p); if (!Number.isFinite(n) || n <= 0) throw new Error(`mids[${i}] must be > 0`); return n; });
}
function parseLines(raw: unknown): string[] {
  if (typeof raw !== "string") return [];
  return raw.split("\n").map((s) => s.trim()).filter((s) => s.length > 0 && !s.startsWith("#"));
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const markets = parseList(body.markets, true);
    if (markets.length === 0) throw new Error("markets required");
    const mids = parseMids(body.mids, markets.length);
    const minConfidence = num(body.minConfidence, "minConfidence");
    if (minConfidence < 0 || minConfidence > 1) throw new Error("minConfidence in [0, 1]");
    const model = typeof body.model === "string" && body.model.trim().length > 0 ? body.model.trim() : "silvana-mock-v1";
    const autoPromptEverySec = Math.max(0, Math.floor(num(body.autoPromptEverySec, "autoPromptEverySec")));
    const prompts = parseLines(body.prompts);
    if (autoPromptEverySec > 0 && prompts.length === 0) throw new Error("prompts pool required when auto-emit is on");

    const config: AiSignalConfig = { markets, mids, minConfidence, model, autoPromptEverySec, prompts };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
