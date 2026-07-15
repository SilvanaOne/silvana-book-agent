// Cheap mean-reverting random walk for cross-venue spread values (in bps).
// Suitable for a UI demo — not for backtesting quality.
//
// Each pair drifts around a per-pair anchor (BASE_BPS) with additive Gaussian
// noise plus a soft pull back toward the anchor, so spreads wander realistically
// without running away to absurd values.

export type WalkParams = Readonly<{
  driftPerTick: number; // constant bps nudge per tick, e.g. 0 = none
  volPerTick: number; // stddev of the per-tick noise, in bps, e.g. 6
}>;

const MIN_BPS = 15;
const MAX_BPS = 820;
const REVERSION = 0.05; // fraction of the gap to the anchor closed each tick

export function clampBps(n: number): number {
  return Math.max(MIN_BPS, Math.min(MAX_BPS, n));
}

/** Standard normal sample via Box-Muller. */
function gauss(): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  return Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
}

export function nextBps(
  current: number,
  base: number,
  params: WalkParams,
  override?: number | null,
): number {
  if (typeof override === "number" && override > 0) return clampBps(override);
  const reversion = (base - current) * REVERSION;
  const next = current + params.driftPerTick + reversion + params.volPerTick * gauss();
  return clampBps(next);
}
