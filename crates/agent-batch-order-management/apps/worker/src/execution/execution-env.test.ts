import assert from "node:assert/strict";
import test from "node:test";

import { executionRouterEnabledFromEnv } from "./execution-env.js";

test("executionRouterEnabledFromEnv: off by default / empty / 0 / false", () => {
  assert.equal(executionRouterEnabledFromEnv({}), false);
  assert.equal(
    executionRouterEnabledFromEnv({
      EXECUTION_ROUTER_ENABLED: "",
    }),
    false,
  );
  assert.equal(
    executionRouterEnabledFromEnv({
      EXECUTION_ROUTER_ENABLED: "0",
    }),
    false,
  );
  assert.equal(
    executionRouterEnabledFromEnv({
      EXECUTION_ROUTER_ENABLED: "false",
    }),
    false,
  );
});

test("executionRouterEnabledFromEnv: truthy aliases", () => {
  assert.equal(
    executionRouterEnabledFromEnv({
      EXECUTION_ROUTER_ENABLED: "1",
    }),
    true,
  );
  assert.equal(
    executionRouterEnabledFromEnv({
      EXECUTION_ROUTER_ENABLED: "true",
    }),
    true,
  );
  assert.equal(
    executionRouterEnabledFromEnv({
      EXECUTION_ROUTER_ENABLED: "YES",
    }),
    true,
  );
  assert.equal(
    executionRouterEnabledFromEnv({
      EXECUTION_ROUTER_ENABLED: "  true  ",
    }),
    true,
  );
});
