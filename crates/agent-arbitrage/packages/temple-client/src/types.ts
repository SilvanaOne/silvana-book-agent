/**
 * Temple market-data response shapes (fields we read).
 *
 * Derived from the `@temple-digital-group/temple-canton-js` documented surface
 * (reference-temple-sdk memory): `getTicker(symbol)` / `getOrderBook(symbol)`.
 * We call the REST market-data endpoint directly with native fetch rather than
 * the SDK (which bundles axios + ws); the existing venue clients all use raw
 * fetch and we only need read-only ticker data here.
 *
 * NOTE: the exact REST path + envelope are best-effort until verified against
 * a live API key — see provider.ts. Mock mode is the Sprint 4 default.
 */
export interface TempleTicker {
  readonly symbol: string;
  /** Best bid price. */
  readonly bid: string;
  /** Best ask price. */
  readonly ask: string;
  /** Last trade price (fallback if bid/ask absent). */
  readonly last?: string;
}
