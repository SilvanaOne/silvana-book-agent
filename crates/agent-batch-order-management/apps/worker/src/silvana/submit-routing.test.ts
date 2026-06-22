import assert from "node:assert/strict";
import test from "node:test";
import type { OrderIntent } from "@batch-order/venue-adapters";

import type { RouterConfig } from "@batch-order/execution-router";
import { chosenVenueForPendingOrderSubmit } from "./submit-routing.js";

function cfg(patch?: Partial<RouterConfig>): RouterConfig {
  return {
    defaultVenue: "silvana",
    maxQtyOkx: "999",
    minQtyRfQ: "1000",
    ...patch,
  };
}

function intent(patch: Partial<OrderIntent>): OrderIntent {
  return {
    market: "ETH-USDC",
    side: "buy",
    type: "limit",
    qty: "1",
    price: "2000",
    ...patch,
  };
}

test("chosenVenueForPendingOrderSubmit: router disabled → persisted venue (rollback path)", () => {
  assert.equal(
    chosenVenueForPendingOrderSubmit({
      executionRouterEnabled: false,
      persistedVenue: "silvana",
      intent: intent({ execProfile: "rfq" }),
      routerCfg: cfg(),
    }),
    "silvana",
    "would route to RFQ lane if router were enabled",
  );
});

test("chosenVenueForPendingOrderSubmit: router disabled respects non-silvana persistence", () => {
  assert.equal(
    chosenVenueForPendingOrderSubmit({
      executionRouterEnabled: false,
      persistedVenue: "okx",
      intent: intent({ market: "canton-cc", qty: "1" }),
      routerCfg: cfg({ defaultVenue: "silvana", minQtyRfQ: "99999" }),
    }),
    "okx",
  );
});

test("chosenVenueForPendingOrderSubmit: router enabled delegates to chooseVenue", () => {
  assert.equal(
    chosenVenueForPendingOrderSubmit({
      executionRouterEnabled: true,
      persistedVenue: "silvana",
      intent: intent({ execProfile: "rfq" }),
      routerCfg: cfg(),
    }),
    "okx-liquid",
  );
});
