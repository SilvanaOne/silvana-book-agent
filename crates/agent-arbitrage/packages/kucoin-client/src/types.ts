/**
 * KuCoin spot API response shapes (only fields we read).
 * Reference: https://www.kucoin.com/docs/rest/spot-trading/market-data/get-ticker
 */

export interface KucoinEnvelope<T> {
  readonly code: string;
  readonly data: T | null;
  readonly msg?: string;
}

export interface KucoinLevel1Ticker {
  readonly sequence: string;
  readonly price: string;
  readonly size: string;
  readonly bestBid: string;
  readonly bestBidSize: string;
  readonly bestAsk: string;
  readonly bestAskSize: string;
  readonly time: number;
}
