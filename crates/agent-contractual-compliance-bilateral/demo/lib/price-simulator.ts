// Cheap random-walk price generator: geometric Brownian motion with
// controllable drift & vol per tick. Suitable for a UI demo — not for
// backtesting quality.

export type WalkParams = Readonly<{
  driftPerTick: number; // e.g. 0.0 = flat, 0.0002 = slight upward drift
  volPerTick: number;   // stddev per tick, e.g. 0.005 = ~0.5%
}>;

export function nextPrice(current: number, params: WalkParams, override?: number | null): number {
  if (typeof override === "number" && override > 0) return override;
  // Box-Muller for a standard normal sample.
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
  const factor = 1 + params.driftPerTick + params.volPerTick * z;
  const next = current * factor;
  return next > 0 ? next : current * 0.5;
}
