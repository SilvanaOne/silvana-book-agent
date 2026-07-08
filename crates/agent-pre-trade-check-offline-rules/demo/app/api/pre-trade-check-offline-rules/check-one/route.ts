import { NextResponse } from "next/server";
import { evaluate, parseRules, DEFAULT_RULES, type Side } from "@/lib/pretradecheck-engine";
import { snapshot } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  try {
    const body = (await req.json()) as Record<string, unknown>;
    const market = String(body.market ?? "").trim();
    if (!market) return NextResponse.json({ error: "market required" }, { status: 400 });

    const sideRaw = String(body.side ?? "").toLowerCase();
    if (sideRaw !== "buy" && sideRaw !== "sell") {
      return NextResponse.json({ error: "side must be 'buy' or 'sell'" }, { status: 400 });
    }
    const side = sideRaw as Side;
    const quantity = num(body.quantity, "quantity");
    const price = num(body.price, "price");
    if (quantity <= 0) return NextResponse.json({ error: "quantity must be > 0" }, { status: 400 });
    if (price <= 0) return NextResponse.json({ error: "price must be > 0" }, { status: 400 });

    // Prefer active rules from the running engine; fall back to inline
    // rulesJson override; finally the built-in defaults.
    const running = snapshot().pretradecheck;
    const rules = running
      ? running.config.rules
      : typeof body.rulesJson === "string"
      ? parseRules(body.rulesJson)
      : DEFAULT_RULES;

    const { decision, failedRules, notional } = evaluate(market, side, quantity, price, rules);
    return NextResponse.json({ decision, failedRules, notional, market, side, quantity, price });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
