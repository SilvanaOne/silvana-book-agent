import { createSilvanaClients, orderbookGrpcConfigFromEnv } from "@batch-order/silvana-client";
import { COMMAND_QUEUE_NAME, Worker, createQueueRedis } from "@batch-order/queue";
import { processCommand } from "./commands-processor.js";
import { workerSettlementSidecarEnabledFromEnv, workerSilvanaModeFromEnv } from "./silvana/exec-mode.js";
import { setWorkerOrderbookClient } from "./silvana/orderbook-holder.js";
import { spawnSilvanaStreamIngest, streamMarketsFromEnv } from "./silvana/stream-consumer.js";

async function main(): Promise<void> {
  const redisUrl = process.env.REDIS_URL?.trim();
  if (!redisUrl) {
    throw new Error("REDIS_URL is required for worker BullMQ consumption");
  }

  const cfg = orderbookGrpcConfigFromEnv();
  const sv = createSilvanaClients(cfg);
  setWorkerOrderbookClient(sv.orderbook);

  console.info(`[worker] Silvana RPC base: ${cfg.baseUrl}`);
  console.info("[worker] Silvana execution mode:", workerSilvanaModeFromEnv());
  if (!cfg.jwt?.trim()) {
    console.warn("[worker] SILVANA_JWT missing — orders stay in DB (plan_only behavior)");
  }
  if (workerSettlementSidecarEnabledFromEnv()) {
    console.info(
      "[worker] Settlement sidecar declared (cloud-agent --settlement-only). " +
        "DVP/multicall выполняет sidecar; worker отслеживает только статусы ордеров. " +
        "См. task/updated-scenario/hybrid-architecture.md.",
    );
  }

  const streamsOff = /^1|true$/i.test(process.env.WORKER_SILVANA_STREAM_DISABLED?.trim() ?? "");
  if (!streamsOff && sv.orderbook) {
    const rawRecon = Number(process.env.WORKER_ORDER_RECONCILE_MS ?? "90000");
    const reconcileMs = Number.isFinite(rawRecon) && rawRecon >= 0 ? Math.floor(rawRecon) : 0;
    const markets = streamMarketsFromEnv();
    spawnSilvanaStreamIngest({ client: sv.orderbook, marketIds: markets, reconcileMs });
    console.info("[worker] Silvana stream ingest started (markets:", markets.join(", ") + `), reconcile_ms=${reconcileMs}`);
  } else if (!sv.orderbook) {
    console.info("[worker] Silvana streams skipped (no orderbook JWT client)");
  } else {
    console.info("[worker] Silvana streams disabled (WORKER_SILVANA_STREAM_DISABLED)");
  }

  try {
    const market = process.env.DEFAULT_MARKET?.trim() ?? "CC-USDC";
    const price = await sv.pricing.getPrice({ marketId: market });
    console.info(`[worker] Pricing sanity OK (${market}), last=${String(price.last)}`);
  } catch (err) {
    console.warn("[worker] pricing sanity skipped or failed:", err instanceof Error ? err.message : err);
  }

  const connection = createQueueRedis(redisUrl);
  const worker = new Worker(COMMAND_QUEUE_NAME, processCommand, {
    concurrency: Number(process.env.WORKER_CONCURRENCY ?? "4"),
    connection,
    autorun: true,
  });

  worker.on("failed", (job, err) => {
    console.error(
      `[worker] job failed (${job?.id ?? "?"}) ${job?.name ?? "?"}`,
      err instanceof Error ? err.message : err,
    );
  });

  console.info("[worker] listening on queue:", COMMAND_QUEUE_NAME);

  const shutdown = async (signal: string) => {
    console.info(`[worker] shutting down (${signal})`);
    await worker.close();
    await connection.quit();
  };

  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.once(signal, () => {
      shutdown(signal)
        .then(() => process.exit(0))
        .catch((err) => {
          console.error(err);
          process.exit(1);
        });
    });
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
