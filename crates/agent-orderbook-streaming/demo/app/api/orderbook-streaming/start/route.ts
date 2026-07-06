import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { ObStreamConfig, PriceSource, SinkKind } from "@/lib/obstream-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

type SinkSpec = { kind: SinkKind; label: string; target?: string };

function parseSinks(raw: unknown): SinkSpec[] {
  if (typeof raw !== "string") throw new Error("sinks must be a string");
  const lines = raw
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0 && !s.startsWith("#"));
  if (lines.length === 0) throw new Error("at least one sink is required");

  const parsed: SinkSpec[] = [];
  for (const line of lines) {
    const [kindRaw, ...rest] = line.split(":");
    const kind = kindRaw.trim().toLowerCase() as SinkKind;
    const target = rest.join(":").trim() || undefined;

    if (kind === "stdout") {
      parsed.push({ kind: "stdout", label: "stdout" });
    } else if (kind === "file") {
      if (!target) throw new Error(`sink "${line}" — file sink requires a path (e.g. file:ticks.jsonl)`);
      parsed.push({ kind: "file", label: `file:${target}`, target });
    } else if (kind === "webhook") {
      if (!target) throw new Error(`sink "${line}" — webhook sink requires a URL (e.g. webhook:https://...)`);
      if (!/^https?:\/\//i.test(target)) throw new Error(`sink "${line}" — webhook URL must start with http(s)://`);
      parsed.push({ kind: "webhook", label: `webhook:${shortUrl(target)}`, target });
    } else {
      throw new Error(`sink "${line}" — unknown kind (expected stdout / file / webhook)`);
    }
  }
  return parsed;
}

function shortUrl(u: string): string {
  try {
    const p = new URL(u);
    return p.host + (p.pathname === "/" ? "" : p.pathname);
  } catch {
    return u;
  }
}

function parseMarkets(raw: unknown): string[] {
  if (typeof raw !== "string") throw new Error("markets must be a comma-separated string");
  const list = raw
    .split(",")
    .map((s) => s.trim().toUpperCase())
    .filter((s) => s.length > 0);
  if (list.length === 0) throw new Error("at least one market is required");
  if (list.length > 6) throw new Error("at most 6 markets are supported in this demo");
  for (const m of list) {
    if (!/^[A-Z0-9]+-[A-Z0-9]+$/.test(m)) throw new Error(`market "${m}" must look like BASE-QUOTE`);
  }
  return list;
}

function parseStartingPrices(raw: unknown, count: number): number[] {
  if (typeof raw !== "string") throw new Error("startingPrices must be a comma-separated string");
  const parts = raw
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
  if (parts.length === 0) throw new Error("startingPrices is required");
  const nums: number[] = parts.map((p, i) => {
    const n = Number(p);
    if (!Number.isFinite(n) || n <= 0) throw new Error(`startingPrices[${i}] must be > 0`);
    return n;
  });
  // Pad by repeating last value if operator provided fewer than markets.
  while (nums.length < count) nums.push(nums[nums.length - 1]);
  return nums.slice(0, count);
}

const VALID_SOURCES: readonly PriceSource[] = ["binance_spot", "bybit", "coingecko", "silvana.oracle"];

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const markets = parseMarkets(body.markets);
    const depth = Math.max(1, Math.min(30, Math.floor(num(body.depth, "depth"))));
    const includeOrderbook = Boolean(body.includeOrderbook);
    const includeTrades = Boolean(body.includeTrades);
    const noDepth = Boolean(body.noDepth);
    const noPrices = Boolean(body.noPrices);
    if (noDepth && noPrices) {
      return NextResponse.json({ error: "nothing to stream — both no-depth and no-prices set" }, { status: 400 });
    }

    const sinks = parseSinks(body.sinks);
    const startingPrices = parseStartingPrices(body.startingPrices, markets.length);
    const failureRate = num(body.webhookFailureRate, "webhookFailureRate");
    if (failureRate < 0 || failureRate > 1) {
      return NextResponse.json({ error: "webhookFailureRate must be in [0, 1]" }, { status: 400 });
    }
    const sourceRaw = typeof body.source === "string" ? body.source.trim().toLowerCase() : "silvana.oracle";
    const source = (VALID_SOURCES.includes(sourceRaw as PriceSource) ? sourceRaw : "silvana.oracle") as PriceSource;

    const config: ObStreamConfig = {
      markets,
      depth,
      includeOrderbook,
      includeTrades,
      noDepth,
      noPrices,
      sinks,
      startingPrices,
      webhookFailureRate: failureRate,
      source,
    };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
