import assert from "node:assert/strict";
import test from "node:test";
import { analyzeRebalance } from "./rebalance.js";
import { checkDailyEstimatedNotional, checkPlannedOrdersMaxNotional } from "./rebalance-risk.js";
import { D } from "./decimal-util.js";

test("analyzeRebalance: adds implicit quote target and plans buy when underweight asset", () => {
  const r = analyzeRebalance({
    portfolioId: "p1",
    quoteCurrency: "USDC",
    thresholdBps: 10,
    targets: [{ assetSymbol: "CC", weight: 0.5 }],
    positions: [
      { assetSymbol: "CC", qty: "100", price: "0.2", marketValue: "20" },
      { assetSymbol: "USDC", qty: "80", price: "1", marketValue: "80" },
    ],
    slipBpsBuy: 0,
    slipBpsSell: 0,
  });

  assert.equal(r.nav, "100");
  assert.deepEqual(
    r.normalizedTargets.map((x) => x.weight),
    ["0.5", "0.5"],
  );
  assert.ok(r.warnings.some((w) => w.includes("implicit quote")));
  assert.equal(r.current.find((x) => x.assetSymbol === "CC")?.weight, "0.2");
  assert.ok(r.plannedOrders.length >= 1);
  const ccOrder = r.plannedOrders.find((o) => o.assetSymbol === "CC");
  assert.ok(ccOrder);
  assert.equal(ccOrder!.side, "buy");
});

test("analyzeRebalance: skips leg below threshold", () => {
  const r = analyzeRebalance({
    portfolioId: "p2",
    quoteCurrency: "USDC",
    thresholdBps: 5000, // 50% drift required
    targets: [
      { assetSymbol: "CC", weight: 0.5 },
      { assetSymbol: "USDC", weight: 0.5 },
    ],
    positions: [
      { assetSymbol: "CC", qty: "100", price: "1", marketValue: "100" },
      { assetSymbol: "USDC", qty: "100", price: "1", marketValue: "100" },
    ],
  });
  assert.equal(r.plannedOrders.length, 0);
  assert.ok(r.drift.every((d) => d.skippedDueToThreshold));
});

test("analyzeRebalance: rejects unknown position asset", () => {
  assert.throws(() =>
    analyzeRebalance({
      portfolioId: "p3",
      quoteCurrency: "USDC",
      thresholdBps: 1,
      targets: [
        { assetSymbol: "CC", weight: 0.5 },
        { assetSymbol: "USDC", weight: 0.5 },
      ],
      positions: [
        { assetSymbol: "CC", qty: "100", price: "1", marketValue: "100" },
        { assetSymbol: "USDC", qty: "100", price: "1", marketValue: "100" },
        { assetSymbol: "ETH", qty: "1", price: "1", marketValue: "1" },
      ],
    }),
  );
});

test("analyzeRebalance: sell when overweight", () => {
  const r = analyzeRebalance({
    portfolioId: "p4",
    quoteCurrency: "USDC",
    thresholdBps: 1,
    targets: [
      { assetSymbol: "CC", weight: 0.25 },
      { assetSymbol: "USDC", weight: 0.75 },
    ],
    positions: [
      { assetSymbol: "CC", qty: "100", price: "4", marketValue: "400" },
      { assetSymbol: "USDC", qty: "100", price: "1", marketValue: "100" },
    ],
  });
  const sell = r.plannedOrders.find((o) => o.assetSymbol === "CC");
  assert.ok(sell);
  assert.equal(sell!.side, "sell");
});

test("risk: checkPlannedOrdersMaxNotional when limit off", () => {
  assert.equal(checkPlannedOrdersMaxNotional([{ price: "10", qty: "2" }], null), null);
});

test("risk: checkPlannedOrdersMaxNotional exceed", () => {
  const v = checkPlannedOrdersMaxNotional([{ price: "10", qty: "2" }], D("15"));
  assert.ok(v);
  assert.equal(v!.code, "risk_max_order_size");
});

test("risk: checkDailyEstimatedNotional", () => {
  const lim = D("100");
  assert.equal(checkDailyEstimatedNotional("30", "60", lim), null);
  const fail = checkDailyEstimatedNotional("50", "60", lim);
  assert.ok(fail);
  assert.equal(fail!.code, "risk_max_daily_notional");
});
