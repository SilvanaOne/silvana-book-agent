/**
 * OneSwap quote response shape (fields we read).
 *
 * Derived from the `@oneswap/sdk` `client.quotes.get(...)` documented surface
 * (reference-oneswap-sdk memory). We call the REST API directly with native
 * fetch rather than pulling in the SDK — the SDK's license is unconfirmed in
 * its package metadata, and the existing CEX clients all use raw fetch.
 *
 * NOTE: the exact REST path + envelope are best-effort until verified against
 * a live devnet key — see provider.ts. Mock mode is the Sprint 3 default.
 */
export interface OneSwapQuoteResponse {
  /** Amount of `to` token received for the requested `from` amount. */
  readonly outputAmount: string;
  /** Effective rate (to per from), if the API returns it directly. */
  readonly rate?: string;
  readonly priceImpact?: string;
  readonly networkFeeAmount?: string;
  readonly poolFeeAmount?: string;
}
