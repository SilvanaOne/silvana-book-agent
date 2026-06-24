import type { Token } from './token.js';
import type { VenueId } from './venue.js';

export interface Pair {
  readonly base: Token;
  readonly quote: Token;
  readonly venueId: VenueId;
}

export const createPair = (base: Token, quote: Token, venueId: VenueId): Pair =>
  Object.freeze({ base, quote, venueId });

/** Canonical key independent of venue — used to group quotes for spread analysis. */
export const tokenPairKey = (pair: Pair): string =>
  `${pair.base.symbol}/${pair.quote.symbol}`;

/** Venue-scoped key — used when same pair on different venues must be distinguished. */
export const venuePairKey = (pair: Pair): string =>
  `${pair.venueId}:${pair.base.symbol}/${pair.quote.symbol}`;
