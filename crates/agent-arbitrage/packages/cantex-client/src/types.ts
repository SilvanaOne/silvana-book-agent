/**
 * Cantex swap-quote response shape (fields we read).
 *
 * Derived from the Python SDK's `get_swap_quote(sell_amount, sell_instrument,
 * buy_instrument)` documented surface (reference-cantex-sdk memory). Cantex
 * ships a Python-only SDK (Apache-2.0); there is no TS SDK, so we port the
 * read-only REST surface with native fetch. `get_swap_quote` returns an
 * instant AMM price including admin/liquidity/network fees.
 *
 * NOTE: the exact REST path + envelope are best-effort until verified against
 * a live operator key — see provider.ts. Mock mode is the Sprint 5 default.
 */
export interface CantexSwapQuote {
  /** Amount of buy_instrument received for the requested sell_amount. */
  readonly buy_amount: string;
  /** Effective price (buy per sell), if returned directly. */
  readonly price?: string;
  readonly admin_fee?: string;
  readonly liquidity_fee?: string;
  readonly network_fee?: string;
}
