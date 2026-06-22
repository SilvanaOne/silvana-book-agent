import type { OrderbookClient } from "@silvana-one/orderbook";
import { randomUUID } from "node:crypto";
import {
  prisma,
  appendOrderEvent,
  findOrderWithBatch,
  updateOrderExecutionState,
  updateOrderPriceQty,
} from "@batch-order/db";
import { checkPlannedOrdersMaxNotional, parseRiskMaxOrderNotionalFromEnv } from "@batch-order/portfolio-engine";
import { mapSilvanaOrderStatusToDomestic, parseSilvanaOrderId, submitLimitGtc } from "@batch-order/silvana-client";

export async function replaceOrderViaSilvana(
  params: Readonly<{ orderId: string; price: string; qty: string; client: OrderbookClient }>,
): Promise<void> {
  const before = await findOrderWithBatch(params.orderId);
  if (!before) throw new Error("order_not_found");

  const maxOrd = parseRiskMaxOrderNotionalFromEnv();
  const riskLeg = checkPlannedOrdersMaxNotional([{ price: params.price, qty: params.qty }], maxOrd);
  if (riskLeg) throw new Error(`${riskLeg.code}: ${riskLeg.message}`);

  await updateOrderPriceQty(params.orderId, { price: params.price, qty: params.qty });

  const ord = await findOrderWithBatch(params.orderId);
  if (!ord) throw new Error("order_not_found_after_update");

  const prevSid = ord.silvanaOrderId;
  if (prevSid) {
    const cx = await params.client.cancelOrder({ orderId: parseSilvanaOrderId(prevSid) });
    if (!cx.success) throw new Error(cx.message ?? "cancel_before_replace_failed");
    await prisma.order.update({ where: { id: ord.id }, data: { silvanaOrderId: null } });

    await appendOrderEvent({
      orderId: ord.id,
      eventType: "silvana_cancel_before_replace",
      payload: JSON.parse(JSON.stringify({ prevSilvanaOrderId: prevSid })),
    });
  }

  const ref = ord.clientOrderRef ?? randomUUID();
  if (!ord.clientOrderRef) await updateOrderExecutionState(ord.id, { clientOrderRef: ref });

  const resp = await submitLimitGtc({
    client: params.client,
    marketId: ord.market,
    side: ord.side,
    price: params.price,
    quantity: params.qty,
    traderOrderRef: ref,
  });

  if (!resp.success || !resp.order) throw new Error(resp.message ?? "submit_after_replace_failed");

  await updateOrderExecutionState(ord.id, {
    silvanaOrderId: resp.order.orderId.toString(),
    status: mapSilvanaOrderStatusToDomestic(resp.order.status),
  });

  await appendOrderEvent({
    orderId: ord.id,
    eventType: "silvana_replace_submit",
    payload: JSON.parse(JSON.stringify({ silvanaOrderId: resp.order.orderId.toString() })),
  });
}
