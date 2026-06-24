export type VenueId =
  | 'bybit'
  | 'kucoin'
  | 'bitget'
  | 'silvana'
  | 'temple'
  | 'oneswap'
  | 'cantex'
  | 'hyperliquid';

export type VenueType =
  | 'cex-spot'
  | 'cex-perp'
  | 'canton-clob'
  | 'canton-amm'
  | 'perp-dex';

export interface Venue {
  readonly id: VenueId;
  readonly type: VenueType;
  readonly displayName: string;
  readonly enabled: boolean;
}
