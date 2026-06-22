import assert from "node:assert/strict";
import test from "node:test";

import {
  workerSettlementSidecarEnabledFromEnv,
  workerSilvanaModeFromEnv,
} from "./exec-mode.js";

test("workerSilvanaModeFromEnv: plan_only when no JWT and no override", () => {
  assert.equal(workerSilvanaModeFromEnv({}), "plan_only");
});

test("workerSilvanaModeFromEnv: rpc only when SILVANA_JWT is present", () => {
  assert.equal(workerSilvanaModeFromEnv({ SILVANA_JWT: "abc" }), "rpc");
  assert.equal(workerSilvanaModeFromEnv({ SILVANA_JWT: "   " }), "plan_only");
});

test("workerSilvanaModeFromEnv: override=plan_only forces plan_only even with JWT", () => {
  assert.equal(
    workerSilvanaModeFromEnv({
      SILVANA_JWT: "abc",
      WORKER_SILVANA_EXECUTION: "plan_only",
    }),
    "plan_only",
  );
});

test("workerSilvanaModeFromEnv: override=rpc still requires JWT", () => {
  assert.equal(
    workerSilvanaModeFromEnv({ WORKER_SILVANA_EXECUTION: "rpc" }),
    "plan_only",
  );
  assert.equal(
    workerSilvanaModeFromEnv({
      WORKER_SILVANA_EXECUTION: "rpc",
      SILVANA_JWT: "abc",
    }),
    "rpc",
  );
});

test("workerSettlementSidecarEnabledFromEnv: off by default and on falsy values", () => {
  assert.equal(workerSettlementSidecarEnabledFromEnv({}), false);
  for (const v of ["", "0", "false", "no", "off"]) {
    assert.equal(
      workerSettlementSidecarEnabledFromEnv({
        WORKER_SILVANA_SETTLEMENT_SIDECAR: v,
      }),
      false,
      `value=${JSON.stringify(v)}`,
    );
  }
});

test("workerSettlementSidecarEnabledFromEnv: truthy aliases", () => {
  for (const v of ["1", "true", "TRUE", "yes", "On", " on "]) {
    assert.equal(
      workerSettlementSidecarEnabledFromEnv({
        WORKER_SILVANA_SETTLEMENT_SIDECAR: v,
      }),
      true,
      `value=${JSON.stringify(v)}`,
    );
  }
});
