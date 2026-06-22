import type { PortfolioTarget, PositionSnapshot } from "@prisma/client";
import type { PositionInputRow, TargetInputRow } from "@batch-order/portfolio-engine";

export function snapshotsToPositions(rows: ReadonlyArray<PositionSnapshot>): PositionInputRow[] {
  return rows.map((s) => ({
    assetSymbol: s.assetSymbol,
    qty: s.qty.toString(),
    price: s.price.toString(),
    marketValue: s.marketValue.toString(),
  }));
}

export function decodeTargets(bodyTargets: unknown): TargetInputRow[] | null {
  if (bodyTargets === undefined || bodyTargets === null) return null;
  if (!Array.isArray(bodyTargets)) throw new Error("targets must be an array");
  return bodyTargets.map((row, idx) => {
    if (!row || typeof row !== "object") throw new Error(`targets[${idx}] invalid`);

    const r = row as Record<string, unknown>;
    const assetSymbol =
      typeof r.assetSymbol === "string" ? r.assetSymbol.trim() : String(r.assetSymbol ?? "").trim();

    if (!assetSymbol) throw new Error(`targets[${idx}].assetSymbol required`);

    if (typeof r.weight !== "number" && typeof r.weight !== "string") throw new Error(`targets[${idx}].weight invalid`);

    const w = typeof r.weight === "number" ? r.weight : Number(r.weight);
    if (!Number.isFinite(w) || w < 0 || w > 1) throw new Error(`targets[${idx}].weight out of range`);

    let enabled = true;

    const enabledRaw = r.enabled;
    if (typeof enabledRaw === "boolean") enabled = enabledRaw;
    else if (typeof enabledRaw === "string") enabled = enabledRaw.toLowerCase() !== "false";

    return { assetSymbol, weight: w, enabled };
  });
}

export function targetsFromPortfolio(targets: ReadonlyArray<PortfolioTarget>): TargetInputRow[] {
  return targets
    .filter((t) => t.enabled)
    .map((t) => ({
      assetSymbol: t.assetSymbol,
      weight: t.targetWeight.toString(),
      enabled: true,
    }));
}

export function parseThresholdBps(body: Record<string, unknown>, envFallback: number): number {
  if (typeof body.thresholdBps === "undefined") return envFallback;

  const n = typeof body.thresholdBps === "number" ? body.thresholdBps : Number(body.thresholdBps);

  if (!Number.isFinite(n) || n < 0) throw new Error("thresholdBps must be a finite non-negative number");
  return n;
}

export function slipBpsFromEnv(): { buy: number; sell: number } {
  const raw = Number(process.env.RISK_MAX_SLIPPAGE_BPS ?? "50");

  const slip = Number.isFinite(raw) && raw >= 0 ? Math.floor(raw) : 50;
  return { buy: slip, sell: slip };
}
