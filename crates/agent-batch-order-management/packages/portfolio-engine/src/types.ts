export interface TargetInputRow {
  assetSymbol: string;
  /** Доля портфеля 0–1 (до нормализации шумов округления). */
  weight: string | number;
  /** Если false — строка не участвует в цели. */
  enabled?: boolean;
}

export interface PositionInputRow {
  assetSymbol: string;
  qty: string | number;
  price: string | number;
  /** Если задано, должно быть согласовано с qty×price (допускается погрешность). */
  marketValue?: string | number;
}

export type PlannedOrder = Readonly<{
  market: string;
  /** Площадка исполнения (опционально; иначе воркер подставляет `WORKER_DEFAULT_EXECUTION_VENUE` / Silvana). */
  venue?: string;
  /** Политика исполнения для execution-router (`chooseVenue` / RFQ-поток). */
  execProfile?: "book" | "rfq" | "block" | "otc";
  side: "buy" | "sell";
  type: "limit";
  qty: string;
  price: string;
  assetSymbol: string;
}>;

export type DriftRow = Readonly<{
  assetSymbol: string;
  targetWeight: string;
  currentWeight: string;
  driftWeightBps: string;
  skippedDueToThreshold: boolean;
}>;

export type AnalyzeRebalanceParams = Readonly<{
  portfolioId: string;
  /** Валюта котировки рынков (`CC-USDC` → USDC). */
  quoteCurrency: string;
  /** Минимальный |Δвеса| для сделки, в basis points портфеля (1 bps = 0.01%). */
  thresholdBps: number;
  targets: TargetInputRow[];
  positions: PositionInputRow[];
  /** Явный market id; иначе `{asset}-{quote}`. */
  marketByAsset?: Partial<Record<string, string>>;
  /** Округление количества (ROUND_DOWN). */
  qtyDecimalPlaces?: number;
  slipBpsBuy?: number;
  slipBpsSell?: number;
  /** Допустимая относительная ошибка marketValue vs qty×price. */
  mvToleranceBps?: number;
}>;

export type AnalyzeRebalanceResult = Readonly<{
  portfolioId: string;
  quoteCurrency: string;
  nav: string;
  normalizedTargets: ReadonlyArray<Readonly<{ assetSymbol: string; weight: string }>>;
  current: ReadonlyArray<Readonly<{ assetSymbol: string; weight: string }>>;
  drift: ReadonlyArray<DriftRow>;
  plannedOrders: ReadonlyArray<PlannedOrder>;
  /** Сумма |Δвес|×NAV по исполненным ногам (строка Decimal). */
  estimatedNotional: string;
  warnings: ReadonlyArray<string>;
}>;
