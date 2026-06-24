import type { VenueId } from './venue.js';

export interface DexQuote {
  readonly venueId: VenueId;
  readonly buyPrice: number;
  readonly sellPrice: number;
  readonly timestamp: number;
  readonly poolAddress?: string;
}
