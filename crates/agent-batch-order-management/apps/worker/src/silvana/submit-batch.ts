import { randomUUID } from "node:crypto";
import type { OrderbookClient } from "@silvana-one/orderbook";
import type { VenueName, OrderIntent, OrderResult } from "@batch-order/venue-adapters";
import type { Decimal } from "@prisma/client/runtime/library";
import type { OrderStatus } from "@batch-order/silvana-client";
import {
  prisma,
  appendAuditLog,
  appendOrderEvent,
  listPendingSubmitOrdersByBatch,
  updateOrderBatch,
  updateOrderExecutionState,
} from "@batch-order/db";
import { passesRiskRules, routerConfigFromEnv } from "@batch-order/execution-router";
import { checkPlannedOrdersMaxNotional, parseRiskMaxOrderNotionalFromEnv } from "@batch-order/portfolio-engine";
import { classifySilvanaError, mapSilvanaOrderStatusToDomestic } from "@batch-order/silvana-client";
import { createWorkerAdapterMap } from "../execution/adapter-registry.js";
import { workerExecutionJsonLog } from "../lib/execution-observability.js";
import { chosenVenueForPendingOrderSubmit } from "./submit-routing.js";

type PendingOrd = Readonly<{
  id: string;
  venue: string;
  execProfile: string | null;
  market: string;
  side: string;
  type: string;
  price: Decimal | null;
  qty: Decimal;
  clientOrderRef: string | null;
  silvanaOrderId: string | null;
  venueOrderRef: string | null;
}>;

function routingIntentWithoutVenue(ord: PendingOrd, clientRef: string): OrderIntent {
  const ep = ord.execProfile?.trim();
  return {
    market: ord.market,
    side: ord.side === "sell" ? "sell" : "buy",
    type: ord.type === "market" ? "market" : "limit",
    qty: ord.qty.toString(),
    price: ord.price?.toString(),
    clientOrderRef: clientRef,
    ...(ep === "rfq" || ep === "block" || ep === "book" || ep === "otc" ? { execProfile: ep } : {}),
  };
}

function domesticStatusFromVenueSubmit(venue: VenueName, result: OrderResult): string {
  if (venue !== "silvana") return "active";

  const raw = result.raw as { order?: { status?: unknown } } | undefined;
  const st = raw?.order?.status;
  if (typeof st === "number") {
    return mapSilvanaOrderStatusToDomestic(st as OrderStatus);
  }
  return "active";
}

export async function submitPendingOrdersInBatch(
  params: Readonly<{
    batchId: string;
    rebalanceJobId: string;
    client: OrderbookClient;
    executionRouterEnabled: boolean;
    bullJobId?: string;
  }>,
): Promise<void> {
  const pending = (await listPendingSubmitOrdersByBatch(params.batchId)) as PendingOrd[];

  if (pending.length === 0) return;

  await updateOrderBatch(params.batchId, { status: "submitting" });

  workerExecutionJsonLog({
    phase: "venue.submit.batch_enter",
    jobId: params.rebalanceJobId,
    bullJobId: params.bullJobId,
    batchId: params.batchId,
  });

  const adapters = createWorkerAdapterMap(params.client);
  const routerCfg = routerConfigFromEnv();

  for (const ord of pending) {
    if (ord.venueOrderRef || ord.silvanaOrderId) continue;

    let clientRef = ord.clientOrderRef ?? randomUUID();
    if (!ord.clientOrderRef) await updateOrderExecutionState(ord.id, { clientOrderRef: clientRef });

    const priceStr = ord.price?.toString();
    if (!priceStr) {
      workerExecutionJsonLog({
        level: "error",
        phase: "venue.submit.validation",

        jobId: params.rebalanceJobId,
        bullJobId: params.bullJobId,

        venue: ord.venue,
        batchId: params.batchId,
        orderId: ord.id,

        order_outcome: "submit_precheck_missing_price",

      });
      await updateOrderExecutionState(ord.id, { status: "submit_failed" });
      await updateOrderBatch(params.batchId, { status: "failed" });
      throw new Error(`missing limit price for order ${ord.id}`);
    }

    const maxOrd = parseRiskMaxOrderNotionalFromEnv();
    const riskLeg = checkPlannedOrdersMaxNotional([{ price: priceStr, qty: ord.qty.toString() }], maxOrd);
    if (riskLeg) {
      workerExecutionJsonLog({
        level: "error",
        phase: "venue.submit.risk_precheck",

        jobId: params.rebalanceJobId,
        bullJobId: params.bullJobId,

        venue: ord.venue,
        batchId: params.batchId,
        orderId: ord.id,

        order_outcome: "risk_max_order_blocked",

        error: riskLeg.message,

      });
      await updateOrderExecutionState(ord.id, { status: "submit_failed" });
      await updateOrderBatch(params.batchId, { status: "failed" });
      await appendOrderEvent({
        orderId: ord.id,
        eventType: "risk_max_order_size",
        payload: JSON.parse(JSON.stringify(riskLeg)),
      });
      await appendAuditLog({
        actorId: "worker",
        action: "order.submit.risk_blocked",
        entityType: "order_batch",
        entityId: params.batchId,
        payload: { rebalanceJobId: params.rebalanceJobId, orderId: ord.id, ...riskLeg },
      });
      throw new Error(riskLeg.message);
    }

    const routingIntent = routingIntentWithoutVenue(ord, clientRef);

    const chosenVenue: VenueName = chosenVenueForPendingOrderSubmit({
      executionRouterEnabled: params.executionRouterEnabled,
      persistedVenue: ord.venue,
      intent: routingIntent,
      routerCfg,
    });

    if (chosenVenue.trim() !== ord.venue.trim()) {
      workerExecutionJsonLog({
        level: "error",
        phase: "venue.route.conflict",

        jobId: params.rebalanceJobId,
        bullJobId: params.bullJobId,

        venue: chosenVenue,
        batchId: params.batchId,
        orderId: ord.id,

        order_outcome: "route_conflict",
        error: `persisted ${ord.venue} vs routed ${chosenVenue}`,

      });
      await updateOrderExecutionState(ord.id, { status: "submit_failed" });
      await appendAuditLog({
        actorId: "worker",
        action: "venue.route.conflict",
        entityType: "order",
        entityId: ord.id,
        payload: {
          rebalanceJobId: params.rebalanceJobId,
          persistedVenue: ord.venue,
          routedVenue: chosenVenue,
        },
      });
      await updateOrderBatch(params.batchId, { status: "failed" });
      throw new Error(`router selected ${chosenVenue} but order row is venue=${ord.venue}`);
    }

    if (params.executionRouterEnabled) {
      await appendAuditLog({
        actorId: "worker",
        action: "venue.route.selected",
        entityType: "order",
        entityId: ord.id,
        payload: JSON.parse(
          JSON.stringify({
            venue: chosenVenue,
            intent: {
              market: routingIntent.market,
              side: routingIntent.side,
              type: routingIntent.type,
              qty: routingIntent.qty,
              ...(routingIntent.execProfile ? { execProfile: routingIntent.execProfile } : {}),
            },
            routedFrom: "router",
            rebalanceJobId: params.rebalanceJobId,
          }),
        ),
      });
    }

    const submitIntent: OrderIntent = { ...routingIntent, venue: chosenVenue };

    const riskAdapter = passesRiskRules(submitIntent, chosenVenue, {});

    if (!riskAdapter.ok) {
      workerExecutionJsonLog({
        level: "error",
        phase: "venue.risk.per_venue_blocked",

        jobId: params.rebalanceJobId,
        bullJobId: params.bullJobId,

        venue: chosenVenue,

        batchId: params.batchId,
        orderId: ord.id,

        order_outcome: "per_venue_risk_blocked",
        ...(submitIntent.execProfile ? { execProfile: submitIntent.execProfile } : {}),

        error: riskAdapter.message,

      });
      await updateOrderExecutionState(ord.id, { status: "submit_failed" });
      await updateOrderBatch(params.batchId, { status: "failed" });
      await appendAuditLog({
        actorId: "worker",
        action: "venue.risk_blocked",
        entityType: "order",
        entityId: ord.id,
        payload: {
          venue: chosenVenue,
          code: riskAdapter.code,
          message: riskAdapter.message,
          rebalanceJobId: params.rebalanceJobId,
          ...(submitIntent.execProfile ? { execProfile: submitIntent.execProfile } : {}),
        },
      });
      throw new Error(riskAdapter.message);
    }

    const tAdapter0 = performance.now();
    try {
      const adapter = adapters[chosenVenue];
      const result = await adapter.submitOrder(submitIntent);
      const latency_ms = Math.round(performance.now() - tAdapter0);

      await updateOrderExecutionState(ord.id, {
        ...(chosenVenue === "silvana"
          ? { silvanaOrderId: result.orderId, venueOrderRef: result.orderId }
          : { venueOrderRef: result.orderId }),
        status: domesticStatusFromVenueSubmit(chosenVenue, result),
      });

      await prisma.orderBatch.update({
        where: { id: params.batchId },
        data: { submittedOrders: { increment: 1 } },
      });

      await appendOrderEvent({
        orderId: ord.id,
        eventType: chosenVenue === "silvana" ? "silvana_submit" : "venue_submit",
        payload: JSON.parse(JSON.stringify({ venue: chosenVenue, orderId: result.orderId })),
      });

      await appendAuditLog({
        actorId: "worker",
        action: "venue.submit",
        entityType: "order",
        entityId: ord.id,
        payload: JSON.parse(
          JSON.stringify({
            venue: chosenVenue,
            venueOrderRef: result.orderId,

            rebalanceJobId: params.rebalanceJobId,
            ...(submitIntent.execProfile ? { execProfile: submitIntent.execProfile } : {}),
          }),
        ),
      });


      workerExecutionJsonLog({
        phase: "venue.adapter.submit_ok",
        jobId: params.rebalanceJobId,
        bullJobId: params.bullJobId,
        venue: chosenVenue,
        batchId: params.batchId,
        orderId: ord.id,

        latency_ms,

        order_outcome: result.status,
        ...(submitIntent.execProfile ? { execProfile: submitIntent.execProfile } : {}),
      });
    } catch (rawErr: unknown) {
      const classified = classifySilvanaError(rawErr);
      const failLatency_ms = Math.round(performance.now() - tAdapter0);

      workerExecutionJsonLog({
        level: "error",
        phase: "venue.adapter.submit_failed",
        jobId: params.rebalanceJobId,
        bullJobId: params.bullJobId,
        venue: chosenVenue,
        batchId: params.batchId,
        orderId: ord.id,
        latency_ms: failLatency_ms,
        order_outcome: "failed",
        ...(submitIntent.execProfile ? { execProfile: submitIntent.execProfile } : {}),
        error: classified.message,
      });

      await updateOrderExecutionState(ord.id, { status: "submit_failed" });

      await appendOrderEvent({
        orderId: ord.id,
        eventType: "venue_submit_error",
        payload: JSON.parse(JSON.stringify(classified)),
      });

      await updateOrderBatch(params.batchId, { status: "failed" });

      await appendAuditLog({
        actorId: "worker",
        action: "batch.submit.failed",
        entityType: "order_batch",
        entityId: params.batchId,
        payload: JSON.parse(
          JSON.stringify({
            rebalanceJobId: params.rebalanceJobId,
            orderId: ord.id,
            venue: chosenVenue,
            ...(submitIntent.execProfile ? { execProfile: submitIntent.execProfile } : {}),
            latency_ms: failLatency_ms,
            ...classified,
          }),
        ),
      });

      throw rawErr;
    }
  }

  await updateOrderBatch(params.batchId, { status: "pending" });
}
