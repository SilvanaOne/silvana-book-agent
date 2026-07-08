import { NextResponse } from "next/server";
import { startReadinessCheck } from "@/lib/store";
import type { ReadinessCheckConfig, RequiredBalance } from "@/lib/readinesscheck-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseRequired(raw: unknown): RequiredBalance[] {
  const s = typeof raw === "string" ? raw : JSON.stringify(raw ?? "");
  let parsed: unknown;
  try {
    parsed = JSON.parse(s);
  } catch {
    throw new Error("requiredBalances must be valid JSON");
  }
  if (!Array.isArray(parsed)) throw new Error("requiredBalances must be an array");
  const out: RequiredBalance[] = [];
  for (const el of parsed) {
    if (!el || typeof el !== "object") throw new Error("each requiredBalance must be {instrument, minAmount}");
    const rec = el as Record<string, unknown>;
    const instrument = typeof rec.instrument === "string" ? rec.instrument.trim() : "";
    const minAmount = num(rec.minAmount, "minAmount");
    if (!instrument) throw new Error("requiredBalance.instrument must be a non-empty string");
    if (minAmount < 0) throw new Error(`requiredBalance.minAmount for ${instrument} must be >= 0`);
    out.push({ instrument, minAmount });
  }
  if (out.length === 0) throw new Error("requiredBalances must contain at least one entry");
  return out;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const requiredBalances = parseRequired(body.requiredBalances);
    const maxFailedSettlements = num(body.maxFailedSettlements ?? 0, "maxFailedSettlements");
    const maxPendingSettlements = num(body.maxPendingSettlements ?? 10, "maxPendingSettlements");
    const checkIntervalSecs = num(body.checkIntervalSecs ?? 10, "checkIntervalSecs");
    const startingPrice = num(body.startingPrice ?? 0.15, "startingPrice");
    const initialFailed = num(body.initialFailed ?? 0, "initialFailed");
    const initialPending = num(body.initialPending ?? 2, "initialPending");

    if (maxFailedSettlements < 0) return NextResponse.json({ error: "maxFailedSettlements must be >= 0" }, { status: 400 });
    if (maxPendingSettlements < 0) return NextResponse.json({ error: "maxPendingSettlements must be >= 0" }, { status: 400 });
    if (checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (initialFailed < 0) return NextResponse.json({ error: "initialFailed must be >= 0" }, { status: 400 });
    if (initialPending < 0) return NextResponse.json({ error: "initialPending must be >= 0" }, { status: 400 });

    // Seed synthetic balances at 1.5x the required minAmount so the first
    // check starts READY; drift will slowly move things around.
    const initialBalances = requiredBalances.map((r) => ({ instrument: r.instrument, balance: r.minAmount * 1.5 }));

    const config: ReadinessCheckConfig = {
      requiredBalances,
      maxFailedSettlements: Math.floor(maxFailedSettlements),
      maxPendingSettlements: Math.floor(maxPendingSettlements),
      requirePreapproval: Boolean(body.requirePreapproval ?? true),
      checkIntervalSecs,
      startingPrice,
      initialBalances,
      initialFailed: Math.floor(initialFailed),
      initialPending: Math.floor(initialPending),
      initialPreapproval: true,
      driftEnabled: Boolean(body.driftEnabled ?? true),
    };

    const state = startReadinessCheck(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
