import { Redis } from "ioredis";

/** Соединение для BullMQ: `maxRetriesPerRequest` должен быть `null`. */
export function createQueueRedis(url: string): Redis {
  return new Redis(url, { maxRetriesPerRequest: null });
}
