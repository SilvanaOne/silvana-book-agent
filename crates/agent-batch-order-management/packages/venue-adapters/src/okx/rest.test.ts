import assert from "node:assert/strict";
import test from "node:test";
import { okxSignBas64 } from "./rest.js";
import { toOkxSpotInstId } from "./inst.js";

test("okxSignBas64 deterministic", () => {
  const a = okxSignBas64({
    secretKey: "test-secret",
    timestamp: "2020-12-08T09:08:57.715Z",
    method: "POST",
    requestPath: "/api/v5/trade/order",
    body: '{"instId":"BTC-USDT"}',
  });
  const b = okxSignBas64({
    secretKey: "test-secret",
    timestamp: "2020-12-08T09:08:57.715Z",
    method: "POST",
    requestPath: "/api/v5/trade/order",
    body: '{"instId":"BTC-USDT"}',
  });
  assert.equal(a, b);
  assert.ok(a.length > 16);
});

test("toOkxSpotInstId uppercases and trims", () => {
  assert.equal(toOkxSpotInstId("  btc-usdt  "), "BTC-USDT");
});
