import type { OrderbookClient } from "@silvana-one/orderbook";
import type { VenueAdapter, VenueName } from "@batch-order/venue-adapters";
import {
  resolveBinanceOtcVenueAdapterFromEnv,
  resolveOkxLiquidVenueAdapterFromEnv,
  createSilvanaVenueAdapter,
  resolveOkxVenueAdapterFromEnv,
  resolveTempleVenueAdapterFromEnv,
} from "@batch-order/venue-adapters";

/** Реестр адаптеров исполнения (Этап 4 апгрейда). */
export function createWorkerAdapterMap(client: OrderbookClient): Record<VenueName, VenueAdapter> {
  return {
    silvana: createSilvanaVenueAdapter({ client }),
    okx: resolveOkxVenueAdapterFromEnv(process.env),
    temple: resolveTempleVenueAdapterFromEnv(process.env),
    "okx-liquid": resolveOkxLiquidVenueAdapterFromEnv(process.env),
    "binance-otc": resolveBinanceOtcVenueAdapterFromEnv(process.env),
  };
}
