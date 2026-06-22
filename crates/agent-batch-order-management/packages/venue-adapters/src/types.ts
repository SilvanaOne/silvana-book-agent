/**
 * Общие контракты multi-venue execution (Этап 1 плана апгрейда).
 * Совместимо с portfolio-engine: количества и цены — Decimal как строка.
 */

export type Side = "buy" | "sell";

export type OrderType = "limit" | "market";

/** Расширяемое перечисление; Binance OTC — отдельный контур комплаенса (Этап 8). */
export type VenueName = "silvana" | "okx" | "temple" | "okx-liquid" | "binance-otc";

/** Унифицированное намерение после планирования (без транспортных деталей venue). */
export type OrderIntent = Readonly<{
  /** Если задано — роутер не переопределяет venue (forced route). */
  venue?: VenueName;
  market: string;
  side: Side;
  type: OrderType;
  qty: string;
  price?: string;
  clientOrderRef?: string;
  /** Стратегический тег для политики: book | rfq | block | otc → Binance OTC. */
  execProfile?: "book" | "rfq" | "block" | "otc";
}>;

export type OrderLifecycleStatus = "accepted" | "rejected" | "filled" | "partial" | "cancelled";

export type OrderResult = Readonly<{
  venue: VenueName;
  /** Стабильный внешний id (RFQ id, Silvana id, OKX ordId…). */
  orderId: string;
  status: OrderLifecycleStatus;
  raw?: unknown;
}>;

/** Минимальный контракт адаптера venue; методы расширяются optional-сигнатурами позже. */
export interface VenueAdapter {
  readonly venue: VenueName;

  submitOrder(intent: OrderIntent): Promise<OrderResult>;
  cancelOrder(params: Readonly<{ orderId: string; market?: string }>): Promise<void>;
  replaceOrder(orderId: string, next: OrderIntent): Promise<OrderResult>;

  getMarketData?(market: string): Promise<unknown>;
  getPositions?(): Promise<unknown>;
  subscribeOrderEvents?(handler: (event: unknown) => void): Promise<void> | void;
}
