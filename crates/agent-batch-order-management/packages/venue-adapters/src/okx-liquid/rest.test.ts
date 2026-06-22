import assert from "node:assert/strict";
import test from "node:test";
import { OkxLiquidHttpClient } from "./rest.js";

test("OkxLiquidHttpClient.rfqPayloadFromIntent maps intent fields", () => {
  const c = new OkxLiquidHttpClient({ baseUrl: "https://liquid.example/" });
  const p = c.rfqPayloadFromIntent(
    {
      market: " BTC-USDT ",
      side: "sell",
      type: "limit",
      qty: " 12.5 ",
      price: "1",
      execProfile: "rfq",
    },
    "ext-1",
  );
  assert.equal(p.market, "BTC-USDT");
  assert.equal(p.side, "sell");
  assert.equal(p.qty, "12.5");
  assert.equal(p.execProfile, "rfq");
  assert.equal(p.externalRef, "ext-1");
});

test("OkxLiquidHttpClient.submitRfqRequest is not wired yet", async () => {
  const c = new OkxLiquidHttpClient({ baseUrl: "https://liquid.example/" });
  await assert.rejects(() => c.submitRfqRequest("/rfq"));
});
