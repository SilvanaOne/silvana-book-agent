import type { OrderIntent, VenueName } from "@batch-order/venue-adapters";
import type { RouterConfig } from "@batch-order/execution-router";
import { chooseVenue } from "@batch-order/execution-router";

/**
 * Venue для submit после планирования: при `EXECUTION_ROUTER_ENABLED=0` сохраняем venue из строки ордера
 * (откат — без `chooseVenue`). При включённом роутере — `chooseVenue(intent, cfg)`.
 */
export function chosenVenueForPendingOrderSubmit(params: Readonly<{
  executionRouterEnabled: boolean;
  persistedVenue: string;
  intent: OrderIntent;
  routerCfg: RouterConfig;
}>): VenueName {
  if (params.executionRouterEnabled) {
    return chooseVenue(params.intent, params.routerCfg);
  }
  return params.persistedVenue as VenueName;
}
