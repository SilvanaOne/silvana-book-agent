import { randomUUID } from "node:crypto";
import type {
  BatchCancelJobData,

  CommandJobPayload,

  Job,

  OrderReplaceJobData,

  RebalanceExecuteJobData,

  RfqFillJobData,

} from "@batch-order/queue";
import { COMMAND_JOB_NAMES } from "@batch-order/queue";
import type { Prisma } from "@batch-order/db";
import {


  prisma,





  appendAuditLog,




  createDraftOrdersInBatch,




  createOrderBatch,




  findPortfolioById,




  findRebalanceJobById,




  findOrderBatchById,




  findOrderWithBatch,




  listOrderBatchesByRebalanceJob,




  listOrdersByBatch,




  updateOrderBatch,




  updateOrderPriceQty,




  updateRebalanceJob,




  sumCompletedLiveEstimatedNotionalUtcDay,

} from "@batch-order/db";


import {


  analyzeRebalance,




  checkDailyEstimatedNotional,




  checkPlannedOrdersMaxNotional,




  parseRiskMaxDailyNotionalFromEnv,




  parseRiskMaxOrderNotionalFromEnv,




  type PlannedOrder,




  type PositionInputRow,




  type TargetInputRow,




} from "@batch-order/portfolio-engine";


import { workerSilvanaModeFromEnv } from "./silvana/exec-mode.js";


import { getWorkerOrderbookClient } from "./silvana/orderbook-holder.js";


import { cancelOrderBatchViaSilvana } from "./silvana/cancel-batch.js";


import { replaceOrderViaSilvana } from "./silvana/replace-order.js";


import { submitPendingOrdersInBatch } from "./silvana/submit-batch.js";
import { handleRfqFill } from "./silvana/rfq-handler.js";
import { workerExecutionJsonLog } from "./lib/execution-observability.js";
import { executionRouterEnabledFromEnv } from "./execution/execution-env.js";


function jsonSerializable<T>(value: T): Prisma.InputJsonValue {




  return JSON.parse(JSON.stringify(value)) as Prisma.InputJsonValue;


}


function slipBpsFromEnv(): { buy: number; sell: number } {




  const raw = Number(process.env.RISK_MAX_SLIPPAGE_BPS ?? "50");


  const slip = Number.isFinite(raw) && raw >= 0 ? Math.floor(raw) : 50;


  return { buy: slip, sell: slip };


}


type ExecuteStoredInput = Readonly<{
  kind: "execute";
  portfolioId: string;


  thresholdBps: number;
  dryRun?: boolean;


  targets: ReadonlyArray<Readonly<{ assetSymbol: string; weight: string | number; enabled?: boolean }>>;



  positions: ReadonlyArray<PositionInputRow>;


}>;


/** Разделитель ключа группировки `{venue, market}` (unit separator не встречается в id). */
const VENUE_MARKET_GROUP_SEP = "\u001f";

/** Значение по умолчанию для всех planned-ног без `venue`; позже можно заменить роутингом по рынку. */
function defaultExecutionVenueFromEnv(): string {
  const raw = process.env.WORKER_DEFAULT_EXECUTION_VENUE?.trim();
  return raw && raw.length > 0 ? raw : "silvana";
}

function resolvePlannedOrderVenue(order: PlannedOrder, fallback: string): string {
  const v = typeof order.venue === "string" ? order.venue.trim() : "";
  return v.length > 0 ? v : fallback;
}

/** Один batch на `{venue, market}`; инвариант: все ордера в batch делят общий venue. */
function groupPlannedOrdersByVenueAndMarket(
  planned: ReadonlyArray<PlannedOrder>,
  defaultVenue: string,
): ReadonlyArray<readonly [venue: string, market: string, legs: PlannedOrder[]]> {
  const map = new Map<string, { venue: string; market: string; legs: PlannedOrder[] }>();

  for (const o of planned) {
    const venue = resolvePlannedOrderVenue(o, defaultVenue);
    const key = `${venue}${VENUE_MARKET_GROUP_SEP}${o.market}`;
    let row = map.get(key);
    if (!row) {
      row = { venue, market: o.market, legs: [] };
      map.set(key, row);
    }
    row.legs.push(o);
  }

  return [...map.values()].map((r) => [r.venue, r.market, r.legs] as const);
}


function coerceTargets(targets: ExecuteStoredInput["targets"]): TargetInputRow[] {


  return [...targets].map((t) => ({


    assetSymbol: String(t.assetSymbol),




    weight: typeof t.weight === "number" ? t.weight : String(t.weight),



    enabled: typeof t.enabled === "boolean" ? t.enabled : true,


  }));


}


async function cancelBatchLocalOnly(batchId: string): Promise<void> {


  const orders = await listOrdersByBatch(batchId);





  await updateOrderBatch(batchId, { status: "cancelling" });





  for (const ord of orders) {


    if (ord.status === "cancelled" || ord.status === "filled" || ord.status === "expired") continue;



    await prisma.order.update({ where: { id: ord.id }, data: { status: "cancelled" } });



    await prisma.orderBatch.update({


      where: { id: batchId },




      data: { cancelledOrders: { increment: 1 } },


    });


  }






  await updateOrderBatch(batchId, { status: "cancelled" });


}


async function executeRebalanceJob(
  jobRowId: string,
  ctx?: Readonly<{ bullJobId?: string }>,
): Promise<void> {


  const row = await findRebalanceJobById(jobRowId);


  if (!row) throw new Error(`rebalance job not found: ${jobRowId}`);



  if (row.status === "completed") return;



  if (row.status !== "queued" && row.status !== "running") {


    throw new Error(`rebalance job is not runnable (status=${row.status})`);


  }



  if (row.status === "queued") {


    await updateRebalanceJob(row.id, { status: "running", startedAt: new Date() });


  }



  workerExecutionJsonLog({
    phase: "rebalance.job.dispatch",
    jobId: row.id,
    bullJobId: ctx?.bullJobId,
  });

  const inputUnknown = row.input;


  if (!inputUnknown || typeof inputUnknown !== "object") {


    throw new Error("rebalance job input invalid");


  }


  const input = inputUnknown as unknown as ExecuteStoredInput;


  if (input.kind !== "execute") throw new Error("unsupported job input kind");


  if (input.portfolioId !== row.portfolioId) throw new Error("portfolioId mismatch");





  const portfolio = await findPortfolioById(row.portfolioId);





  if (!portfolio) throw new Error("portfolio_not_found");





  const slip = slipBpsFromEnv();





  const targets = coerceTargets(input.targets);



  const result = analyzeRebalance({


    portfolioId: row.portfolioId,





    quoteCurrency: portfolio.baseCurrency,





    thresholdBps: Number.isFinite(input.thresholdBps) ? input.thresholdBps : 100,




    targets,






    positions: [...input.positions],

    slipBpsBuy: slip.buy,

    slipBpsSell: slip.sell,
  });

  const dryRun = Boolean(row.dryRun);

  if (!dryRun && result.plannedOrders.length > 0) {
    const maxOrder = parseRiskMaxOrderNotionalFromEnv();
    const orderFail = checkPlannedOrdersMaxNotional(result.plannedOrders, maxOrder);
    if (orderFail) {
      await updateRebalanceJob(row.id, {
        status: "failed",
        finishedAt: new Date(),
        output: jsonSerializable({
          phase: "risk_blocked",
          violation: orderFail,
          portfolioEngine: result,
        }),
      });
      await appendAuditLog({
        actorId: "worker",
        action: "rebalance.risk_blocked",
        entityType: "rebalance_job",
        entityId: row.id,
        payload: { code: orderFail.code, message: orderFail.message },
      });
      return;
    }

    const maxDaily = parseRiskMaxDailyNotionalFromEnv();
    if (maxDaily) {
      const used = await sumCompletedLiveEstimatedNotionalUtcDay(row.portfolioId, { excludeJobId: row.id });
      const dayFail = checkDailyEstimatedNotional(result.estimatedNotional, used.toString(), maxDaily);
      if (dayFail) {
        await updateRebalanceJob(row.id, {
          status: "failed",
          finishedAt: new Date(),
          output: jsonSerializable({
            phase: "risk_blocked",
            violation: dayFail,
            portfolioEngine: result,
          }),
        });
        await appendAuditLog({
          actorId: "worker",
          action: "rebalance.risk_blocked",
          entityType: "rebalance_job",
          entityId: row.id,
          payload: { code: dayFail.code, message: dayFail.message },
        });
        return;
      }
    }
  }

  const mode = workerSilvanaModeFromEnv();
  const oc = getWorkerOrderbookClient();

  if (dryRun || result.plannedOrders.length === 0) {


    await updateRebalanceJob(row.id, {
      status: "completed",
      finishedAt: new Date(),
      output: jsonSerializable({
        phase: dryRun ? "dry_run_completed" : "no_orders_planned",
        executionMode: mode,
        portfolioEngine: result,
      }),
    });





    await appendAuditLog({
      actorId: "worker",

      action: "rebalance.completed",
      entityType: "rebalance_job",
      entityId: row.id,

      payload: { dryRun, plannedOrders: result.plannedOrders.length },
    });
    return;
  }




  let batchIds: string[] = [];





  const existing = await listOrderBatchesByRebalanceJob(row.id);





  if (existing.length > 0) {




    batchIds = existing.map((b) => b.id);



  } else {




    const defaultVenue = defaultExecutionVenueFromEnv();

    for (const [venue, market, legs] of groupPlannedOrdersByVenueAndMarket(result.plannedOrders, defaultVenue)) {




      const { id: batchId } = await createOrderBatch({
        rebalanceJobId: row.id,
        status: "pending_submit",
        venue,
        market,

      });





      batchIds.push(batchId);




      await createDraftOrdersInBatch(


        batchId,


        legs.map((o) => ({
          venue,

          side: o.side,

          type: o.type,

          market: o.market,

          ...(typeof o.execProfile === "string" && o.execProfile.length > 0 ? { execProfile: o.execProfile } : {}),

          price: o.price,

          qty: o.qty,

          clientOrderRef: randomUUID(),

        })),



      );





    }






  }





  const execRouterOn = executionRouterEnabledFromEnv();



  if (mode === "rpc" && oc) {




    try {




      for (const bid of batchIds) {




        await submitPendingOrdersInBatch({
          batchId: bid,
          rebalanceJobId: row.id,
          client: oc,

          executionRouterEnabled: execRouterOn,
          bullJobId: ctx?.bullJobId,
        });





      }




      await updateRebalanceJob(row.id, {
        status: "completed",
        finishedAt: new Date(),
        output: jsonSerializable({
          phase: execRouterOn ? "execution_router_submit" : "silvana_submitted",
          executionMode: mode,

          executionRouterEnabled: execRouterOn,

          portfolioEngine: result,

          batchIds,

        }),
      });


    } catch (e: unknown) {


      await updateRebalanceJob(row.id, {
        status: "failed",
        finishedAt: new Date(),
        output: jsonSerializable({
          phase: "submit_failed",
          executionMode: mode,
          portfolioEngine: result,

          batchIds,

          error: e instanceof Error ? e.message : String(e),
        }),
      });





      await appendAuditLog({
        actorId: "worker",

        action: "rebalance.failed",
        entityType: "rebalance_job",
        entityId: row.id,
        payload: { message: e instanceof Error ? e.message : String(e) },
      });







      throw e;





    }





  } else {





    await updateRebalanceJob(row.id, {


      status: "completed",





      finishedAt: new Date(),
      output: jsonSerializable({
        phase: "planned_and_persisted",
        executionMode: mode,

        portfolioEngine: result,

        batchIds,

        note: "Нет JWT или включён plan_only — ордера только в PostgreSQL",

      }),

    });

  }







  await appendAuditLog({
    actorId: "worker",

    action: "rebalance.completed",
    entityType: "rebalance_job",

    entityId: row.id,


    payload: {
      dryRun: false,

      batches: batchIds.length,

      plannedOrders: result.plannedOrders.length,




      executionMode: mode,




    },


  });




}


async function cancelBatch(job: Readonly<{ orderBatchId: string }>): Promise<void> {


  const batch = await findOrderBatchById(job.orderBatchId);


  if (!batch) throw new Error("batch_not_found");





  const mode = workerSilvanaModeFromEnv();





  const oc = getWorkerOrderbookClient();





  if (mode === "rpc" && oc) await cancelOrderBatchViaSilvana({ batchId: job.orderBatchId, client: oc });








  else await cancelBatchLocalOnly(job.orderBatchId);



  await appendAuditLog({
    actorId: "worker",

    action: "batch.cancel.completed",
    entityType: "order_batch",

    entityId: job.orderBatchId,

    payload: { rebalanceJobId: batch.rebalanceJobId, executionMode: mode },


  });




}


async function replaceOrder(job: Readonly<OrderReplaceJobData>): Promise<void> {


  const order = await findOrderWithBatch(job.orderId);





  if (!order) throw new Error("order_not_found");





  const mode = workerSilvanaModeFromEnv();





  const oc = getWorkerOrderbookClient();





  if (mode === "rpc" && oc)


    await replaceOrderViaSilvana({ orderId: job.orderId, price: job.price, qty: job.qty, client: oc });








  else await updateOrderPriceQty(job.orderId, { price: job.price, qty: job.qty });








  await appendAuditLog({


    actorId: "worker",


    action: "order.replace.done",





    entityType: "order",





    entityId: job.orderId,





    payload: { rebalanceJobId: order.orderBatch.rebalanceJobId, batchId: order.orderBatch.id, executionMode: mode },


  });




}


export async function processCommand(job: Job<CommandJobPayload>): Promise<void> {


  switch (job.name as (typeof COMMAND_JOB_NAMES)[keyof typeof COMMAND_JOB_NAMES]) {


    case COMMAND_JOB_NAMES.REBALANCE_EXECUTE: {


      await executeRebalanceJob((job.data as RebalanceExecuteJobData).rebalanceJobId, {
          bullJobId: job.id != null ? String(job.id) : undefined,
        });



      return;


    }





    case COMMAND_JOB_NAMES.BATCH_CANCEL: {


      await cancelBatch(job.data as BatchCancelJobData);





      return;


    }







    case COMMAND_JOB_NAMES.ORDER_REPLACE: {


      await replaceOrder(job.data as OrderReplaceJobData);



      return;


    }




    case COMMAND_JOB_NAMES.RFQ_FILL: {


      await handleRfqFill(job.data as RfqFillJobData, {
        bullJobId: job.id != null ? String(job.id) : undefined,
      });



      return;


    }





    default: {


      throw new Error(`unknown job name: ${String(job.name)}`);

    }





  }






}



