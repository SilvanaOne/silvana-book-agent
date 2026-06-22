import assert from "node:assert/strict";
import test from "node:test";

import { workerExecutionJsonLog } from "./execution-observability.js";

test("workerExecutionJsonLog: emits one JSON line with component worker", () => {
  const lines: string[] = [];
  const prevInfo = console.info;
  console.info = (...args: unknown[]) => lines.push(args.map(String).join(" "));
  try {
    workerExecutionJsonLog({ phase: "test.phase", venue: "silvana" });
    assert.equal(lines.length, 1);
    const parsed = JSON.parse(lines[0]!) as Record<string, unknown>;
    assert.equal(parsed.component, "worker");
    assert.equal(parsed.level, "info");
    assert.equal(parsed.phase, "test.phase");
    assert.equal(parsed.venue, "silvana");
    assert.ok(typeof parsed.ts === "string");
  } finally {
    console.info = prevInfo;
  }
});

test("workerExecutionJsonLog: error level uses stderr", () => {
  const lines: string[] = [];
  const prevErr = console.error;
  console.error = (...args: unknown[]) => lines.push(args.map(String).join(" "));
  try {
    workerExecutionJsonLog({ level: "error", phase: "fail", error: "x" });
    assert.equal(lines.length, 1);
    const parsed = JSON.parse(lines[0]!) as Record<string, unknown>;
    assert.equal(parsed.level, "error");
    assert.equal(parsed.error, "x");
  } finally {
    console.error = prevErr;
  }
});
