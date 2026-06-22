import type { PositionInputRow, TargetInputRow } from "./types.js";
import { D } from "./decimal-util.js";

/** Нормализует веса активных целей к сумме 1; при необходимости добавляет quote как остаток. */
export function resolveTargetWeights(
  targets: TargetInputRow[],
  quoteCurrency: string,
): { weights: Map<string, unknown>; warnings: string[] } {
  const warnings: string[] = [];
  const raw = new Map<string, unknown>();

  for (const row of targets) {
    if (row.enabled === false) continue;
    const sym = row.assetSymbol.trim();
    if (!sym) throw new Error("target.assetSymbol must be non-empty");
    if (raw.has(sym)) throw new Error(`duplicate target for ${sym}`);
    const w = D(row.weight) as any;

    if (w.isNeg() || w.gt(1)) throw new Error(`target weight for ${sym} out of range [0,1]: ${w.toFixed()}`);
    raw.set(sym, w);
  }

  if (raw.size === 0) throw new Error("no active targets");

  const hasQuote = raw.has(quoteCurrency);

  if (hasQuote) {
    let sumAll: any = D(0);
    for (const w of raw.values()) sumAll = sumAll.add(w);
    const one = D(1) as any;

    if (!sumAll.eq(one)) throw new Error(`target weights must sum to 1, got ${sumAll.toFixed()}`);
    return { weights: raw, warnings };
  }

  let sumRisky: any = D(0);
  for (const [sym, w] of raw) {
    if (sym !== quoteCurrency) sumRisky = sumRisky.add(w);
  }

  const oneAny = D(1) as any;
  if (sumRisky.gt(oneAny)) throw new Error(`sum of non-quote targets exceeds 1: ${sumRisky.toFixed()}`);

  const quoteW = oneAny.sub(sumRisky);

  raw.set(quoteCurrency, quoteW);
  warnings.push(`implicit quote target for ${quoteCurrency} = ${quoteW.toFixed()} (1 − sum(other targets))`);
  return { weights: raw, warnings };
}

export function buildPositionMap(
  positions: PositionInputRow[],
  mvToleranceBps: number,
): Map<string, { qty: any; price: any; mv: any }> {
  const map = new Map<string, { qty: any; price: any; mv: any }>();
  const tol: any = D(mvToleranceBps).div(10000);

  for (const p of positions) {
    const sym = p.assetSymbol.trim();
    if (!sym) throw new Error("position.assetSymbol must be non-empty");
    if (map.has(sym)) throw new Error(`duplicate position for ${sym}`);
    const qty: any = D(p.qty);

    const price: any = D(p.price);

    if (qty.isNeg()) throw new Error(`negative qty for ${sym}`);

    if (price.lte(0)) throw new Error(`non-positive price for ${sym}`);

    const implied: any = qty.mul(price);
    let mv: any = implied;
    if (p.marketValue !== undefined) {
      mv = D(p.marketValue);

      if (mv.isNeg()) throw new Error(`negative marketValue for ${sym}`);

      let maxAbs: any = implied.abs();
      const mAbs = mv.abs();
      if (mAbs.gt(maxAbs)) maxAbs = mAbs;
      const floor: any = D("1e-18");
      if (floor.gt(maxAbs)) maxAbs = floor;
      const rel: any = implied.sub(mv).abs().div(maxAbs);
      if (rel.gt(tol)) {
        throw new Error(`marketValue inconsistent with qty×price for ${sym} (tolerance ${mvToleranceBps} bps)`);
      }
    }
    map.set(sym, { qty, price, mv });
  }
  return map;
}

export function validatePositionUniverse(
  targetKeys: Set<string>,
  quoteCurrency: string,
  posKeys: Set<string>,
): void {
  for (const sym of posKeys) {
    if (!targetKeys.has(sym)) {
      throw new Error(
        `position for ${sym} is not in targets (add target or remove position). Quote is ${quoteCurrency}.`,
      );
    }
  }
}

export { D, toDecimal } from "./decimal-util.js";
