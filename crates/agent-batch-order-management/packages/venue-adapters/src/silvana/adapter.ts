import type { OrderbookClient } from "@silvana-one/orderbook";
import type { OrderIntent, OrderResult, VenueAdapter } from "../types.js";
import { classifySilvanaError, parseSilvanaOrderId, submitLimitGtc } from "@batch-order/silvana-client";

export type SilvanaVenueAdapterDeps = Readonly<{ client: OrderbookClient }>;

export function createSilvanaVenueAdapter(deps: SilvanaVenueAdapterDeps): VenueAdapter {
  return {
    venue: "silvana",
    async submitOrder(intent: OrderIntent): Promise<OrderResult> {
      if (intent.type !== "limit") {
        throw new Error(`silvana: only limit orders supported (got ${intent.type})`);
      }
      const price = intent.price?.trim();
      if (!price?.length) {
        throw new Error("silvana: limit order requires intent.price");
      }
      try {
        const resp = await submitLimitGtc({
          client: deps.client,
          marketId: intent.market,
          side: intent.side,
          price,
          quantity: intent.qty.trim(),
          traderOrderRef: intent.clientOrderRef ?? undefined,
        });
        if (!resp.success || !resp.order) {
          throw new Error(resp.message?.trim().length ? resp.message : "submitLimitGtc unsuccessful");
        }
        return {
          venue: "silvana",
          orderId: resp.order.orderId.toString(),
          status: "accepted",
          raw: resp,
        };
      } catch (err: unknown) {
        const cls = classifySilvanaError(err);
        throw new Error(`silvana submitOrder: ${cls.message}`);
      }
    },
    async cancelOrder(paramsOrder: Readonly<{ orderId: string; market?: string }>): Promise<void> {
      void paramsOrder.market;
      const resp = await deps.client.cancelOrder({
        orderId: parseSilvanaOrderId(paramsOrder.orderId),
      });
      if (!resp.success) {
        throw new Error(resp.message?.trim().length ? resp.message : "cancelOrder unsuccessful");
      }
    },
    async replaceOrder(_orderId: string, _next: OrderIntent): Promise<OrderResult> {
      throw new Error("@batch-order/venue-adapters: Silvana replaceOrder — use worker replace-order path");
    },
  };
}
