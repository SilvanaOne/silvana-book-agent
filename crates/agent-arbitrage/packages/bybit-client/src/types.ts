/**
 * Bybit V5 API response shapes (only fields we read).
 * Reference: https://bybit-exchange.github.io/docs/v5/market/tickers
 */

export interface BybitV5Envelope<T> {
  readonly retCode: number;
  readonly retMsg: string;
  readonly result: T;
  readonly time: number;
}

export interface BybitSpotTicker {
  readonly symbol: string;
  readonly bid1Price: string;
  readonly ask1Price: string;
  readonly lastPrice: string;
}

export interface BybitSpotTickersResult {
  readonly category: 'spot';
  readonly list: readonly BybitSpotTicker[];
}
