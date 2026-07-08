import { NextResponse } from "next/server";
import { startNotifier } from "@/lib/store";
import type { NotifierConfig, SinkKind } from "@/lib/notifier-engine";

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
      if (!target) throw new Error(`sink "${line}" — file sink requires a path (e.g. file:events.jsonl)`);
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

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const orders = Boolean(body.orders);
    const settlements = Boolean(body.settlements);
    const prices = Boolean(body.prices);
    if (!orders && !settlements && !prices) {
      return NextResponse.json({ error: "at least one stream must be enabled (orders / settlements / prices)" }, { status: 400 });
    }
    const sinks = parseSinks(body.sinks);
    const marketRaw = typeof body.market === "string" ? body.market.trim() : "";
    const failureRate = num(body.webhookFailureRate, "webhookFailureRate");
    if (failureRate < 0 || failureRate > 1) {
      return NextResponse.json({ error: "webhookFailureRate must be in [0, 1]" }, { status: 400 });
    }
    const startingPrice = num(body.startingPrice, "startingPrice");
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });

    const config: NotifierConfig = {
      orders,
      settlements,
      prices,
      sinks,
      market: marketRaw.length > 0 ? marketRaw : undefined,
      startingPrice,
      webhookFailureRate: failureRate,
    };
    const state = startNotifier(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
