import assert from "node:assert/strict";
import test from "node:test";

import {
  buildRfqCommand,
  rfqCliConfigFromEnv,
  validateRfqPayload,
} from "./rfq-cli.js";
import type { RfqFillJobData } from "@batch-order/queue";

function dockerCfg(overrides?: Partial<{ dryRunDefault: boolean; container: string; agentDir: string }>) {
  return {
    transport: {
      kind: "docker" as const,
      container: overrides?.container ?? "bam-cloud-agent",
      agentDir: overrides?.agentDir ?? "/agent",
    },
    dryRunDefault: overrides?.dryRunDefault ?? true,
    timeoutMs: 900_000,
  };
}

function hostCfg(overrides?: Partial<{ dryRunDefault: boolean; binPath: string; agentDir: string }>) {
  return {
    transport: {
      kind: "host" as const,
      binPath: overrides?.binPath ?? "/usr/local/bin/cloud-agent",
      agentDir: overrides?.agentDir ?? "/srv/agent",
    },
    dryRunDefault: overrides?.dryRunDefault ?? true,
    timeoutMs: 900_000,
  };
}

const minPayload: RfqFillJobData = {
  side: "buy",
  marketId: "CC-USDC",
  amount: "1.0",
};

test("rfqCliConfigFromEnv: returns null when nothing is configured", () => {
  assert.equal(rfqCliConfigFromEnv({}), null);
});

test("rfqCliConfigFromEnv: docker by container name (autodetect)", () => {
  const cfg = rfqCliConfigFromEnv({ WORKER_RFQ_DOCKER_CONTAINER: "bam-cloud-agent" });
  assert.ok(cfg);
  assert.equal(cfg!.transport.kind, "docker");
  if (cfg!.transport.kind === "docker") {
    assert.equal(cfg!.transport.container, "bam-cloud-agent");
    assert.equal(cfg!.transport.agentDir, "/agent");
  }
  assert.equal(cfg!.dryRunDefault, true, "safe default is dry-run");
});

test("rfqCliConfigFromEnv: host transport requires both bin and dir", () => {
  assert.equal(
    rfqCliConfigFromEnv({ WORKER_RFQ_TRANSPORT: "host", WORKER_RFQ_BIN_PATH: "/x/cloud-agent" }),
    null,
    "no agent dir → null",
  );
  const cfg = rfqCliConfigFromEnv({
    WORKER_RFQ_TRANSPORT: "host",
    WORKER_RFQ_BIN_PATH: "/x/cloud-agent",
    WORKER_RFQ_AGENT_DIR: "/y",
  });
  assert.ok(cfg);
  assert.equal(cfg!.transport.kind, "host");
});

test("rfqCliConfigFromEnv: explicit dry_run=0 flips default", () => {
  const cfg = rfqCliConfigFromEnv({
    WORKER_RFQ_DOCKER_CONTAINER: "c",
    WORKER_RFQ_DRY_RUN_DEFAULT: "0",
  });
  assert.equal(cfg!.dryRunDefault, false);
});

test("buildRfqCommand: docker transport puts --dry-run BEFORE subcommand", () => {
  const built = buildRfqCommand({ cfg: dockerCfg({ dryRunDefault: true }), payload: minPayload });
  assert.equal(built.cmd, "docker");
  const idxDryRun = built.args.indexOf("--dry-run");
  const idxSub = built.args.indexOf("buy");
  assert.ok(idxDryRun >= 0 && idxDryRun < idxSub, `expected --dry-run before buy; got: ${built.display}`);
  assert.equal(built.dryRun, true);
  assert.deepEqual(built.args.slice(0, 4), ["exec", "-w", "/agent", "bam-cloud-agent"]);
  assert.ok(built.args.includes("--market"));
  assert.ok(built.args.includes("CC-USDC"));
  assert.ok(built.args.includes("--amount"));
  assert.ok(built.args.includes("1.0"));
});

test("buildRfqCommand: explicit payload.dryRun=false overrides cfg default", () => {
  const built = buildRfqCommand({
    cfg: dockerCfg({ dryRunDefault: true }),
    payload: { ...minPayload, dryRun: false },
  });
  assert.equal(built.dryRun, false);
  assert.equal(built.args.includes("--dry-run"), false);
});

test("buildRfqCommand: host transport adds --config <dir>/agent.toml", () => {
  const built = buildRfqCommand({
    cfg: hostCfg({ binPath: "/opt/sa/cloud-agent", agentDir: "/var/sa/" }),
    payload: { ...minPayload, side: "sell" },
  });
  assert.equal(built.cmd, "/opt/sa/cloud-agent");
  assert.deepEqual(built.args.slice(0, 2), ["--config", "/var/sa/agent.toml"]);
  assert.ok(built.args.includes("sell"));
});

test("buildRfqCommand: optional flags propagate", () => {
  const built = buildRfqCommand({
    cfg: dockerCfg({ dryRunDefault: false }),
    payload: {
      side: "buy",
      marketId: "CC-USDC",
      amount: "10",
      priceLimit: "0.16",
      minSettlement: "1.5",
      maxSettlement: "5",
      intervalSec: 30,
    },
  });
  for (const tok of ["--price-limit", "0.16", "--min-settlement", "1.5", "--max-settlement", "5", "--interval", "30"]) {
    assert.ok(built.args.includes(tok), `missing ${tok} in ${built.display}`);
  }
});

test("validateRfqPayload: rejects bad side", () => {
  assert.throws(
    () =>
      validateRfqPayload({ ...minPayload, side: "swap" as unknown as RfqFillJobData["side"] }),
    /unsupported side/,
  );
});

test("validateRfqPayload: rejects non-positive amount", () => {
  assert.throws(() => validateRfqPayload({ ...minPayload, amount: "0" }), /positive number/);
  assert.throws(() => validateRfqPayload({ ...minPayload, amount: "-1" }), /positive number/);
  assert.throws(() => validateRfqPayload({ ...minPayload, amount: "abc" }), /positive number/);
});

test("validateRfqPayload: rejects negative numeric flags", () => {
  assert.throws(
    () => validateRfqPayload({ ...minPayload, minSettlement: "-1" }),
    /minSettlement/,
  );
  assert.throws(
    () => validateRfqPayload({ ...minPayload, intervalSec: -1 }),
    /intervalSec/,
  );
});

test("validateRfqPayload: accepts minimal valid payload", () => {
  validateRfqPayload(minPayload);
});
