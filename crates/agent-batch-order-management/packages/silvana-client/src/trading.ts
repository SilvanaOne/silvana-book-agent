import { randomInt } from "node:crypto";
import { OrderStatus, OrderType, TimeInForce, OrderbookError, type OrderbookClient } from "@silvana-one/orderbook";

export { OrderStatus, OrderType, OrderbookError, TimeInForce };

export function mapBuySellSideToOrderType(side: string): OrderType {
  const s = side.trim().toLowerCase();

  if (s === "buy") return OrderType.BID;

  if (s === "sell") return OrderType.OFFER;

  throw new Error(`unsupported order side: "${side}" (expected buy|sell)`);
}

/** Nonce replay protection для SubmitOrder (`uint64`). */
export function randomSubmitNonce(): bigint {
  return BigInt(randomInt(1_000_000, 9_000_000_000));
}

export async function submitLimitGtc(params: Readonly<{ client: OrderbookClient; marketId: string; side: string; price: string; quantity: string; traderOrderRef?: string }>) {
  return params.client.submitOrder({
    marketId: params.marketId,
    orderType: mapBuySellSideToOrderType(params.side),
    price: params.price,

    quantity: params.quantity,

    timeInForce: TimeInForce.GTC,

    traderOrderRef: params.traderOrderRef ?? undefined,

    nonce: randomSubmitNonce(),
  });
}

/** Парсит id ордера в book для `cancelOrder({ orderId: bigint })`. */
export function parseSilvanaOrderId(orderIdStr: string): bigint {
  return BigInt(orderIdStr.trim());
}

/** Мапинг Silvana `OrderStatus` → строковый статус нашей модели (`orders.status`). */

export function mapSilvanaOrderStatusToDomestic(status: OrderStatus): string {
  switch (status) {

    case OrderStatus.ACTIVE:
      return "active";

    case OrderStatus.PARTIAL:
      return "partial_fill";

    case OrderStatus.FILLED:
      return "filled";


    case OrderStatus.CANCELLED:
      return "cancelled";

    case OrderStatus.EXPIRED:
      return "expired";


    default:
      return "unknown";

  }


}

export function classifySilvanaError(err: unknown): { transient: boolean; message: string; code?: string } {
  if (err instanceof OrderbookError) {
    const transient = err.code === "UNAVAILABLE" || err.code === "UNKNOWN" || /ECONNRESET|ENOTFOUND|EAI_AGAIN/i.test(err.message);

    return { transient, message: err.message, ...(err.code ? { code: err.code } : {}) };
  }


  const message = err instanceof Error ? err.message : String(err);
  const transient = /ECONNRESET|ENOTFOUND|EAI_AGAIN|UNAVAILABLE/i.test(message);

  return { transient, message };

}
