import { createGrpcTransport } from "@connectrpc/connect-node";
import { OrderbookClient, PricingClient } from "@silvana-one/orderbook";
/** Реэкспорт официальных клиентов и типов Silvana (одна точка импорта для apps). */
export {
  LedgerClient,

  OrderbookClient,
  OrderbookError,

  OrderStatus,

  OrderType,

  PricingClient,

  SettlementClient,

  TimeInForce,
} from "@silvana-one/orderbook";
export { createGrpcTransport } from "@connectrpc/connect-node";

export type OrderbookGrpcConfig = Readonly<{
  baseUrl: string;
  /** Bearer JWT для Orderbook/Ledger/Settlement unary/stream (после onboard). */
  jwt?: string;
}>;

export function createSilvanaTransport(baseUrl: string) {
  return createGrpcTransport({ baseUrl });
}

/** Pricing без JWT + Orderbook только если задан непустой `jwt`. */
export function createSilvanaClients(cfg: OrderbookGrpcConfig) {
  const transport = createGrpcTransport({ baseUrl: cfg.baseUrl });
  const pricing = new PricingClient({ transport });
  const token = cfg.jwt?.trim();
  const orderbook =
    token !== undefined && token !== ""
      ? new OrderbookClient({ transport, token })
      : null;
  return { transport, pricing, orderbook };
}

export type SilvanaClientsBundle = ReturnType<typeof createSilvanaClients>;

/**
 * Совместимо с переменными монорепо и типичным выходом cloud-agent онбординга.
 * Приоритет URL: SILVANA_RPC → ORDERBOOK_GRPC_URL → devnet Quickstart default.
 */
export function orderbookGrpcConfigFromEnv(
  env: NodeJS.ProcessEnv = process.env,
): OrderbookGrpcConfig {
  const baseUrl =
    env.SILVANA_RPC?.trim() ||
    env.ORDERBOOK_GRPC_URL?.trim() ||
    "https://orderbook-devnet.silvana.dev:443";
  const jwt = env.SILVANA_JWT?.trim();

  return jwt ? { baseUrl, jwt } : { baseUrl };
}

export {

  mapBuySellSideToOrderType,

  submitLimitGtc,

  classifySilvanaError,

  mapSilvanaOrderStatusToDomestic,

  parseSilvanaOrderId,

  randomSubmitNonce,

} from "./trading.js";
export {


  jsonFromStreamMessage,

  buildOrderStreamEventKey,

  orderUpdateEventLabel,

  settlementStreamEventLabel,

  buildSettlementStreamEventKey,

  resolveOrderBookStatusHint,

} from "./stream-handling.js";
