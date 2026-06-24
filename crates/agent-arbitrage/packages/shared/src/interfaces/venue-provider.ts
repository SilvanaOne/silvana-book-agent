import type { Pair, DexQuote, VenueId } from '../types/index.js';

/**
 * Read-only abstraction for any venue (CEX or DEX).
 *
 * Trade execution is NOT part of this interface — it lives in the proprietary
 * executor engine (separate repo). See md_docs/12-component-split-open-vs-proprietary.md.
 */
export interface IVenueProvider {
  readonly venueId: VenueId;
  readonly name: string;

  /** Fetch buy/sell quotes for a pair. Returns null if pair not supported or fetch fails. */
  getQuote(pair: Pair): Promise<DexQuote | null>;

  /** Check whether this venue supports the given pair. */
  supportsPair(pair: Pair): boolean;
}
