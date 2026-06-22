/** Одна очередь команд для executor (название BullMQ Queue). */
export const COMMAND_QUEUE_NAME = "batch-order-commands";

export const COMMAND_JOB_NAMES = {
  REBALANCE_EXECUTE: "rebalance_execute",
  BATCH_CANCEL: "batch_cancel",
  ORDER_REPLACE: "order_replace",
  /** Гибрид «Вариант Б» (Шаг 4): worker делегирует RFQ-такер `cloud-agent` через CLI. */
  RFQ_FILL: "rfq_fill",
} as const;

export type RebalanceExecuteJobData = Readonly<{ rebalanceJobId: string }>;

export type BatchCancelJobData = Readonly<{ orderBatchId: string }>;

export type OrderReplaceJobData = Readonly<{
  orderId: string;
  price: string;
  qty: string;
}>;

/**
 * RFQ-такер через cloud-agent. Семантика полей соответствует флагам
 * `cloud-agent buy|sell` (см. README silvana-book-agent / `examples/buy_cc`).
 *
 * - `side` — `buy` или `sell`, маппится в подкоманду cloud-agent.
 * - `marketId` — `--market` (например `CC-USDC`).
 * - `amount` — `--amount` (десятичное число; total amount of base instrument).
 * - `priceLimit` — `--price-limit` (max для buy, min для sell). Если не задан,
 *   cloud-agent сам подставляет mid±3%.
 * - `minSettlement` / `maxSettlement` — `--min-settlement` / `--max-settlement`
 *   (chunking RFQ; min по умолчанию 5.0).
 * - `intervalSec` — `--interval` (retry интервал, сек).
 * - `clientRef` — пишется в audit-log как корреляционный ID.
 * - `dryRun` — если true, worker добавляет глобальный `--dry-run` cloud-agent.
 *   По умолчанию TRUE, чтобы в проде нельзя было случайно вызвать RFQ без явного
 *   подтверждения. См. `WORKER_RFQ_DRY_RUN_DEFAULT`.
 */
export type RfqFillJobData = Readonly<{
  side: "buy" | "sell";
  marketId: string;
  amount: string;
  priceLimit?: string;
  minSettlement?: string;
  maxSettlement?: string;
  intervalSec?: number;
  clientRef?: string;
  dryRun?: boolean;
}>;
