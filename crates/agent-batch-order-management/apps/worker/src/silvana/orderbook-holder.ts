import type { OrderbookClient } from "@silvana-one/orderbook";

/** Глобальный контекст: Orderbook клиент живёт только в процессе worker после старта. */
let singleton: OrderbookClient | null = null;


export function setWorkerOrderbookClient(client: OrderbookClient | null): void {
  singleton = client;

}


export function getWorkerOrderbookClient(): OrderbookClient | null {
  return singleton;

}

