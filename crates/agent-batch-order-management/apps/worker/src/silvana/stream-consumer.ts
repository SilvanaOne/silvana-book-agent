import type { OrderbookClient, OrderUpdate, SettlementUpdate } from "@silvana-one/orderbook";
import { OrderStatus } from "@silvana-one/orderbook";
import {
  findOrderBySilvanaOrderIdWithBatch,
  ingestOrderLifecycleFromSilvanaStream,
  ingestSettlementStreamEvent,
} from "@batch-order/db";

import type { Prisma } from "@batch-order/db";
import {

  buildOrderStreamEventKey,
  buildSettlementStreamEventKey,
  classifySilvanaError,
  jsonFromStreamMessage,

  mapSilvanaOrderStatusToDomestic,
  orderUpdateEventLabel,

  resolveOrderBookStatusHint,
  settlementStreamEventLabel,

} from "@batch-order/silvana-client";

const backoffMsDefault = Number(process.env.WORKER_STREAM_BACKOFF_MS ?? "8000");

function subscribeOptsForMarkets(marketIds: string[]): { marketId?: string } {
  return marketIds.length === 1 && marketIds[0]?.trim()
    ? { marketId: marketIds[0]!.trim() }
    : {};
}

async function ingestOrderUpdateEnvelope(u: OrderUpdate): Promise<void> {
  const inner = u.order;
  const sidRaw = inner?.orderId ?? undefined;
  if (sidRaw === undefined) return;

  const sdkStatus = resolveOrderBookStatusHint(u);
  if (sdkStatus === OrderStatus.UNSPECIFIED) return;

  const domestic = mapSilvanaOrderStatusToDomestic(sdkStatus);
  if (domestic === "unknown") return;

  const sid = sidRaw.toString();
  const ver = inner?.version ?? 0n;
  const evtKey = buildOrderStreamEventKey(sid, ver, u.eventType);
  const payload = jsonFromStreamMessage(u) as Prisma.InputJsonValue;

  await ingestOrderLifecycleFromSilvanaStream({
    silvanaOrderId: sid,
    silvanaEventKey: evtKey,

    eventTypeLabel: orderUpdateEventLabel(u.eventType),
    domesticStatusFromBook: domestic,
    payload,
  });
}

async function ingestSettlementEnvelope(s: SettlementUpdate): Promise<void> {
  const p = s.proposal;
  const om = p?.orderMatch;
  if (!p?.proposalId || !om) return;

  const row =
    (await findOrderBySilvanaOrderIdWithBatch(om.bidOrderId.toString())) ??
    (await findOrderBySilvanaOrderIdWithBatch(om.offerOrderId.toString()));
  if (!row?.orderBatch) return;

  await ingestSettlementStreamEvent({
    rebalanceJobId: row.orderBatch.rebalanceJobId,
    silvanaStreamKey: buildSettlementStreamEventKey(p.proposalId, s.eventType),
    eventStatusLabel: settlementStreamEventLabel(s.eventType),

    payload: jsonFromStreamMessage(s) as Prisma.InputJsonValue,
  });

}

async function runOrdersLoop(oc: OrderbookClient, opts: Readonly<{ marketIds: string[]; label: string }>): Promise<void> {
  while (true) {

    try {
      const subOpts = subscribeOptsForMarkets(opts.marketIds);
      for await (const u of oc.subscribeOrders(subOpts)) {
        await ingestOrderUpdateEnvelope(u).catch((e: unknown) => {
          console.warn(`[${opts.label}] order ingest:`, classifySilvanaError(e).message);
        });
      }
      console.warn(`[${opts.label}] subscribeOrders ended → reconnect`);
    } catch (e: unknown) {
      console.warn(`[${opts.label}] subscribeOrders:`, classifySilvanaError(e).message);
    }
    await new Promise((r) => setTimeout(r, backoffMsDefault));
  }

}

async function runSettlementsLoop(oc: OrderbookClient, opts: Readonly<{ marketIds: string[]; label: string }>): Promise<void> {
  while (true) {
    try {
      const subOpts = subscribeOptsForMarkets(opts.marketIds);
      for await (const s of oc.subscribeSettlements(subOpts)) {
        await ingestSettlementEnvelope(s).catch((err: unknown) => {
          console.warn(`[${opts.label}] settlement ingest:`, classifySilvanaError(err).message);
        });
      }
      console.warn(`[${opts.label}] subscribeSettlements ended → reconnect`);
    } catch (e: unknown) {
      console.warn(`[${opts.label}] subscribeSettlements:`, classifySilvanaError(e).message);
    }
    await new Promise((r) => setTimeout(r, backoffMsDefault));
  }

}

async function reconcileMarketOnce(oc: OrderbookClient, marketId: string): Promise<void> {
  try {
    const hist = await oc.getOrderHistory({ marketId, limit: 250 });
    for (const snap of hist.orders ?? []) {
      const domestic = mapSilvanaOrderStatusToDomestic(snap.status);
      if (domestic === "unknown") continue;
      const sid = snap.orderId.toString();
      const evtKey = `ob:reconcile:${sid}:v:${snap.version.toString()}`;
      await ingestOrderLifecycleFromSilvanaStream({
        silvanaOrderId: sid,
        silvanaEventKey: evtKey,

        eventTypeLabel: "silvana_reconcile",
        domesticStatusFromBook: domestic,
        payload: jsonFromStreamMessage(snap) as Prisma.InputJsonValue,
      });
    }
  } catch (e: unknown) {
    console.warn(`[worker] reconcile(${marketId}):`, classifySilvanaError(e).message);
  }
}


export type SilvanaStreamLoopParams = Readonly<{
  client: OrderbookClient;
  marketIds: string[];

  reconcileMs: number;



  label?: string;

}>;


/** Фоновые стримы + reconcile. Не await — держит процесс активным через бесконечные циклы. */
export function spawnSilvanaStreamIngest(p: SilvanaStreamLoopParams): void {
  const mk = [...p.marketIds];

  const label = p.label ?? "silvana-stream";

  void runOrdersLoop(p.client, { marketIds: mk, label }).catch((err) =>
    console.error(`[${label}] orders loop fatal`, err),
  );


  void runSettlementsLoop(p.client, { marketIds: mk, label }).catch((err) =>
    console.error(`[${label}] settlements loop fatal`, err),
  );

  if (p.reconcileMs > 0 && mk.length > 0) {
    const tick = (): void => {
      void Promise.all(mk.map((m) => reconcileMarketOnce(p.client, m)));



    };


    tick();



    setInterval(tick, p.reconcileMs);
  }

}





/**
 * Явный `WORKER_STREAM_MARKET_IDS=` (пустая строка) → подписка без `market_id` (все рынки по JWT).


 * Не задано → один рынок `DEFAULT_MARKET` или CC-USDC.
 */

export function streamMarketsFromEnv(env: NodeJS.ProcessEnv = process.env): string[] {
  if (env.WORKER_STREAM_MARKET_IDS !== undefined) {
    const parts = env.WORKER_STREAM_MARKET_IDS.split(",").map((s) => s.trim()).filter(Boolean);
    return [...new Set(parts)];
  }
  const m = env.DEFAULT_MARKET?.trim() || "CC-USDC";
  return [m];
}
