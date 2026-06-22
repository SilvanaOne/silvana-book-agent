import { Queue } from "bullmq";
import type { Redis } from "ioredis";
import type {
  BatchCancelJobData,
  OrderReplaceJobData,
  RebalanceExecuteJobData,
  RfqFillJobData,
} from "./constants.js";
import { COMMAND_QUEUE_NAME } from "./constants.js";

/** Полезная нагрузка job (тип задаётся отдельно полем `job.name`). */
export type CommandJobPayload =
  | RebalanceExecuteJobData
  | BatchCancelJobData
  | OrderReplaceJobData
  | RfqFillJobData;

/** Очередь команд API → worker. */
export function createCommandsQueue(connection: Redis) {
  return new Queue<CommandJobPayload>(COMMAND_QUEUE_NAME, {
    connection,
    defaultJobOptions: {
      attempts: 3,
      backoff: { type: "exponential", delay: 750 },
      removeOnComplete: true,
      removeOnFail: 500,
    },
  });
}
