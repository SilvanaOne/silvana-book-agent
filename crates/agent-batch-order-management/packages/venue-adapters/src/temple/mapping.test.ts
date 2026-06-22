import assert from "node:assert/strict";
import test from "node:test";
import {
  instrumentIdFallbackFromHyphenMarket,
  normalizeTempleMarketSymbol,
  toTempleInstrumentId,
} from "./mapping.js";

test("normalizeTempleMarketSymbol trims and uppercase", () => {
  assert.equal(normalizeTempleMarketSymbol("  btc-usdt  "), "BTC-USDT");
});

test("toTempleInstrumentId uses alias ETH-USDC", () => {
  assert.equal(toTempleInstrumentId("eth-usdc "), "ETH/USDC");
});

test("toTempleInstrumentId fallback hyphen pairs", () => {
  assert.equal(toTempleInstrumentId("FOO-BAR-X"), instrumentIdFallbackFromHyphenMarket("FOO-BAR-X"));
});
