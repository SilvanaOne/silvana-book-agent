import { Decimal } from "./decimal-cjs.js";
import type { AnalyzeRebalanceParams, AnalyzeRebalanceResult, DriftRow, PlannedOrder } from "./types.js";
import { D } from "./decimal-util.js";
import { buildPositionMap, resolveTargetWeights, validatePositionUniverse } from "./validation.js";

function slipPrice(mid: any, side: "buy" | "sell", slipBps: number): any {
  if (slipBps <= 0) return mid;

  const f: any = D(slipBps).div(10000);
  return side === "buy" ? mid.mul(D(1).add(f)) : mid.mul(D(1).sub(f));
}

function marketId(asset: string, quote: string, override?: string): string {
  return (override?.trim() || `${asset}-${quote}`).trim();
}

/**
 * Чистая функция: целевые веса + снимок позиций → дрейф и план limit-ордеров (MVP).
 */
export function analyzeRebalance(params: AnalyzeRebalanceParams): AnalyzeRebalanceResult {
  const quote = params.quoteCurrency.trim();

  if (!quote) throw new Error("quoteCurrency required");

  const qtyDp = params.qtyDecimalPlaces ?? 12;

  const slipBuy = params.slipBpsBuy ?? 0;

  const slipSell = params.slipBpsSell ?? 0;

  const mvTol = params.mvToleranceBps ?? 50;

  const { weights: targetMapRaw, warnings: wTargets } = resolveTargetWeights([...params.targets], quote);

  const targetMap = targetMapRaw as Map<string, any>;

  const targetKeys = new Set(targetMap.keys());

  const posMapTyped = buildPositionMap([...params.positions], mvTol);
  const posMap = posMapTyped as Map<string, { qty: any; price: any; mv: any }>;

  validatePositionUniverse(targetKeys, quote, new Set(posMap.keys()));

  const warnings = [...wTargets];
  for (const sym of targetKeys) {
    if (!posMap.has(sym)) warnings.push(`no position row for ${sym}; market value assumed 0`);
  }

  let nav: any = D(0);
  for (const sym of targetKeys) {
    const row = posMap.get(sym);

    nav = nav.add(row?.mv ?? D(0));
  }
  if (nav.lte(0)) throw new Error("NAV must be positive");

  const threshold: any = D(params.thresholdBps).div(10000);
  if (threshold.isNeg()) throw new Error("thresholdBps must be non-negative");

  const currentWeights = new Map<string, any>();
  for (const sym of targetKeys) {
    const mv = posMap.get(sym)?.mv ?? D(0);
    currentWeights.set(sym, mv.div(nav));
  }

  const normalizedTargets: Array<{ assetSymbol: string; weight: string }> = [];

  const currentOut: Array<{ assetSymbol: string; weight: string }> = [];

  const driftRows: DriftRow[] = [];

  const planned: PlannedOrder[] = [];

  let estimatedNotional: any = D(0);

  const sortedSyms = [...targetKeys].sort();

  for (const sym of sortedSyms) {
    const tw = targetMap.get(sym)!;
    const cw = currentWeights.get(sym)!;
    normalizedTargets.push({ assetSymbol: sym, weight: tw.toFixed() });

    currentOut.push({ assetSymbol: sym, weight: cw.toFixed() });

    const delta: any = tw.sub(cw);

    const driftBps: any = delta.mul(10000);
    const absDelta: any = delta.abs();
    const skipped = absDelta.lt(threshold);

    driftRows.push({
      assetSymbol: sym,
      targetWeight: tw.toFixed(),
      currentWeight: cw.toFixed(),
      driftWeightBps: driftBps.toFixed(4),
      skippedDueToThreshold: skipped,
    });

    if (sym === quote) continue;

    if (skipped) continue;

    const pxRow = posMap.get(sym);
    if (!pxRow) throw new Error(`cannot plan trade without position/mark price row for asset ${sym}`);

    const mid = pxRow.price;
    if (mid.lte(0)) throw new Error(`invalid mid price for ${sym}`);

    const notional: any = absDelta.mul(nav);

    const side: "buy" | "sell" = delta.gt(0) ? "buy" : "sell";

    const limitPx: any = slipPrice(mid, side, side === "buy" ? slipBuy : slipSell);

    const rawQty: any = notional.div(limitPx);

    const qty: any = rawQty.toDecimalPlaces(qtyDp, Decimal.ROUND_DOWN);

    if (qty.lte(0)) continue;

    estimatedNotional = estimatedNotional.add(notional);

    planned.push({
      market: marketId(sym, quote, params.marketByAsset?.[sym]),
      side,
      type: "limit",
      qty: qty.toFixed(),
      price: limitPx.toFixed(),
      assetSymbol: sym,
    });
  }

  return {
    portfolioId: params.portfolioId,
    quoteCurrency: quote,
    nav: nav.toFixed(),
    normalizedTargets,
    current: currentOut,
    drift: driftRows,
    plannedOrders: planned,
    estimatedNotional: estimatedNotional.toFixed(),
    warnings,
  };
}
