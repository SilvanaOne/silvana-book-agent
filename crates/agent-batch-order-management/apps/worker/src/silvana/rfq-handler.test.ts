import assert from "node:assert/strict";
import test from "node:test";

import type { Prisma } from "@batch-order/db";
import type { RfqFillJobData } from "@batch-order/queue";

import { handleRfqFill, type AuditLogWriter, type RfqFillDeps } from "./rfq-handler.js";
import type { RfqCliConfig } from "./rfq-cli.js";
import type { RfqExecutor, RfqRunResult } from "./rfq-runner.js";

type AuditEntry = Readonly<{
  actorId: string;
  action: string;
  entityType: string;
  entityId: string;
  payload: Prisma.InputJsonValue;
}>;

function newAuditCollector(): { entries: AuditEntry[]; writer: AuditLogWriter } {
  const entries: AuditEntry[] = [];
  const writer: AuditLogWriter = async (p) => {
    entries.push(p);
    return undefined;
  };
  return { entries, writer };
}

function dockerCfg(): RfqCliConfig {
  return {
    transport: { kind: "docker", container: "bam-cloud-agent", agentDir: "/agent" },
    dryRunDefault: true,
    timeoutMs: 1_000,
  };
}

function execOk(stdout = "OK\n"): RfqExecutor {
  return async ({ built }) => ({
    exitCode: 0,
    stdout,
    stderr: "",
    command: built.display,
    timedOut: false,
  });
}

function execFail(exitCode: number, stderr = "boom"): RfqExecutor {
  return async ({ built }) => ({
    exitCode,
    stdout: "",
    stderr,
    command: built.display,
    timedOut: false,
  });
}

function execTimeout(): RfqExecutor {
  return async ({ built }) => ({
    exitCode: -1,
    stdout: "",
    stderr: "",
    command: built.display,
    timedOut: true,
  });
}

const okPayload: RfqFillJobData = { side: "buy", marketId: "CC-USDC", amount: "1.0" };

test("handleRfqFill: misconfigured (configFromEnv → null) → throws + audit entry", async () => {
  const audit = newAuditCollector();
  const deps: RfqFillDeps = {
    configFromEnv: () => null,
    exec: execOk(),
    appendAuditLog: audit.writer,
  };
  await assert.rejects(handleRfqFill(okPayload, deps), /не сконфигурирован/);
  const actions = audit.entries.map((e) => e.action);
  assert.deepEqual(actions, ["rfq.fill.misconfigured"]);
});

test("handleRfqFill: dry-run path writes requested + dry_run_completed and does not throw", async () => {
  const audit = newAuditCollector();
  await handleRfqFill(okPayload, {
    configFromEnv: () => dockerCfg(),
    exec: execOk(),
    appendAuditLog: audit.writer,
  });
  const actions = audit.entries.map((e) => e.action);
  assert.deepEqual(actions, ["rfq.fill.requested", "rfq.fill.dry_run_completed"]);
});

test("handleRfqFill: real submit path (dryRun=false in payload) writes requested + completed", async () => {
  const audit = newAuditCollector();
  await handleRfqFill(
    { ...okPayload, dryRun: false },
    { configFromEnv: () => dockerCfg(), exec: execOk(), appendAuditLog: audit.writer },
  );
  assert.deepEqual(
    audit.entries.map((e) => e.action),
    ["rfq.fill.requested", "rfq.fill.completed"],
  );
});

test("handleRfqFill: non-zero exit → throws + audit `rfq.fill.failed`", async () => {
  const audit = newAuditCollector();
  await assert.rejects(
    handleRfqFill(okPayload, {
      configFromEnv: () => dockerCfg(),
      exec: execFail(2, "verify failed"),
      appendAuditLog: audit.writer,
    }),
    /exit=2/,
  );
  assert.deepEqual(
    audit.entries.map((e) => e.action),
    ["rfq.fill.requested", "rfq.fill.failed"],
  );
});

test("handleRfqFill: timeout → throws + audit `rfq.fill.timeout`", async () => {
  const audit = newAuditCollector();
  await assert.rejects(
    handleRfqFill(okPayload, {
      configFromEnv: () => dockerCfg(),
      exec: execTimeout(),
      appendAuditLog: audit.writer,
    }),
    /timed out/,
  );
  assert.deepEqual(
    audit.entries.map((e) => e.action),
    ["rfq.fill.requested", "rfq.fill.timeout"],
  );
});

test("handleRfqFill: invalid payload (amount=0) fails BEFORE audit write", async () => {
  const audit = newAuditCollector();
  await assert.rejects(
    handleRfqFill(
      { ...okPayload, amount: "0" },
      {
        configFromEnv: () => dockerCfg(),
        exec: execOk(),
        appendAuditLog: audit.writer,
      },
    ),
    /positive number/,
  );
  assert.equal(audit.entries.length, 0, "no audit writes on validation error");
});

test("handleRfqFill: entityId uses clientRef when provided", async () => {
  const audit = newAuditCollector();
  await handleRfqFill(
    { ...okPayload, clientRef: "rebal-7-leg-2" },
    { configFromEnv: () => dockerCfg(), exec: execOk(), appendAuditLog: audit.writer },
  );
  for (const e of audit.entries) {
    assert.equal(e.entityId, "rebal-7-leg-2");
    assert.equal(e.entityType, "rfq_fill");
  }
});
