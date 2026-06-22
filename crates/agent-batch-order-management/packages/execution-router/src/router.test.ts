import assert from "node:assert/strict";
import test from "node:test";
import type { OrderIntent } from "@batch-order/venue-adapters";
import { chooseVenue } from "./routeOrder.js";
import { passesRiskRules } from "./riskRules.js";
import { routerConfigFromEnv } from "./routerConfig.js";

function base(
  cfg?: Partial<{
    defaultVenue: "silvana" | "okx" | "temple" | "okx-liquid" | "binance-otc";
    minQtyRfQ: string;
    maxQtyOkx: string;
    maxQtyLiquid?: string;
  }>,
) {
  return {
    defaultVenue: "silvana" as const,
    maxQtyOkx: "999",
    minQtyRfQ: "1000",
    ...cfg,
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

test("chooseVenue: forced intent.venue wins", () => {
  assert.equal(chooseVenue(intent({ venue: "temple", market: "canton-cc" }), base()), "temple");
});

test("chooseVenue: execProfile rfq → okx-liquid", () => {
  assert.equal(chooseVenue(intent({ execProfile: "rfq" }), base()), "okx-liquid");
});

test("chooseVenue: execProfile block → okx-liquid", () => {
  assert.equal(chooseVenue(intent({ execProfile: "block" }), base()), "okx-liquid");
});

test("chooseVenue: execProfile otc → binance-otc", () => {
  assert.equal(chooseVenue(intent({ execProfile: "otc", type: "limit" }), base()), "binance-otc");
});

test("chooseVenue: market order → okx", () => {
  assert.equal(chooseVenue(intent({ type: "market", price: undefined }), base()), "okx");
});

test("chooseVenue: large numeric qty vs minQtyRfQ → okx-liquid before canton heuristic", () => {
  assert.equal(
    chooseVenue(intent({ market: "CC-Canton", qty: "2000", type: "limit" }), base({ minQtyRfQ: "1000" })),
    "okx-liquid",
  );
});

test("chooseVenue: canton substring in market → temple", () => {
  assert.equal(
    chooseVenue(intent({ market: "CC-USDC-Canton-SPOT", qty: "1" }), base({ minQtyRfQ: "5000" })),
    "temple",
  );
});

test("chooseVenue: fallback defaultVenue", () => {
  assert.equal(
    chooseVenue(intent({ market: "CC-USDC", qty: "1" }), base({ defaultVenue: "okx", minQtyRfQ: "99999" })),
    "okx",
  );
});

test("chooseVenue: trims qty for numeric comparison", () => {
  assert.equal(
    chooseVenue(intent({ qty: "  1500 ", type: "limit" }), base({ minQtyRfQ: "1000" })),
    "okx-liquid",
  );
});

test("chooseVenue: maxQtyLiquid cap skips RFQ heuristic; canton heuristic still wins", () => {
  assert.equal(
    chooseVenue(
      intent({ market: "CC-Canton", qty: "2000", type: "limit" }),
      base({ minQtyRfQ: "1000", maxQtyLiquid: "1500" }),
    ),
    "temple",
  );
});

test("chooseVenue: maxQtyLiquid passes when qty within cap", () => {
  assert.equal(
    chooseVenue(intent({ qty: "2000", type: "limit" }), base({ minQtyRfQ: "1000", maxQtyLiquid: "2500" })),
    "okx-liquid",
  );
});

test("chooseVenue: execProfile rfq ignores maxQtyLiquid ceiling", () => {
  assert.equal(
    chooseVenue(intent({ execProfile: "rfq", qty: "999999", type: "limit" }), base({ maxQtyLiquid: "10" })),
    "okx-liquid",
  );
});

test("routerConfigFromEnv: reads ROUTER_* and aligns with WORKER_DEFAULT_EXECUTION_VENUE fallback", () => {
  const cfg = routerConfigFromEnv({
    ROUTER_DEFAULT_VENUE: "",
    WORKER_DEFAULT_EXECUTION_VENUE: "okx",
    ROUTER_MIN_QTY_RFQ: "10",
    ROUTER_MAX_ORDER_SIZE_OKX: "42",
    ROUTER_MAX_ORDER_SIZE_LIQUID: "1000000",
  });
  assert.equal(cfg.defaultVenue, "okx");
  assert.equal(cfg.minQtyRfQ, "10");
  assert.equal(cfg.maxQtyOkx, "42");
  assert.equal(cfg.maxQtyLiquid, "1000000");
});

test("passesRiskRules: ok when no cap", () => {
  assert.deepEqual(passesRiskRules(intent({}), "silvana", {}), { ok: true });
});

test("passesRiskRules: ok when cap but no estimate (no price)", () => {
  const r = passesRiskRules(intent({ type: "limit", price: undefined }), "silvana", {
    maxPerVenueUsd: { silvana: "1" },
  });
  assert.deepEqual(r, { ok: true });
});

test("passesRiskRules: fails when notionals exceed cap", () => {
  const r = passesRiskRules(intent({ type: "limit", price: "10", qty: "5", market: "X" }), "okx", {
    maxPerVenueUsd: { okx: "40" },
  });
  assert.equal(r.ok, false);
  if (!r.ok) assert.equal(r.code, "per_venue_notional_cap");
});
