import { COMMAND_JOB_NAMES, createCommandsQueue, createQueueRedis } from "@batch-order/queue";
import type { Queue } from "bullmq";
import type { Redis } from "ioredis";

let redis: Redis | null = null;
let queue: Queue | null = null;

export function getCommandsQueue(): { redis: Redis; queue: Queue } {
  const url = process.env.REDIS_URL?.trim();
  if (!url) throw new Error("REDIS_URL is not configured");

  if (!redis) redis = createQueueRedis(url);
  if (!queue) queue = createCommandsQueue(redis);
  return { redis, queue };
}

export { COMMAND_JOB_NAMES };
