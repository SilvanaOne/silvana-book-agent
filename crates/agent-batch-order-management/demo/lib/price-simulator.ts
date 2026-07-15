// Cheap random-walk price generator: geometric Brownian motion with
// controllable drift & vol per tick. Suitable for a UI demo — not for
// backtesting quality. Off by default (vol 0 = static prices); nudge the
// vol control to make the drift table breathe between rebalances.

export type WalkParams = Readonly<{
  driftPerTick: number; // e.g. 0.0 = flat, 0.0002 = slight upward drift
  volPerTick: number; // stddev per tick, e.g. 0.004 = ~0.4%
}>;

export function nextPrice(current: number, params: WalkParams): number {
  if (params.volPerTick <= 0 && params.driftPerTick === 0) return current;
  // Box-Muller for a standard normal sample.
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
  const factor = 1 + params.driftPerTick + params.volPerTick * z;
  const next = current * factor;
  return next > 0 ? next : current * 0.5;
}
