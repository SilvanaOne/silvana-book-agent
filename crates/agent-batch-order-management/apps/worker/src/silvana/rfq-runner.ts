/**
 * Шаг 4 / Вариант Б: «грязная» часть RFQ-bridge — spawn подпроцесса,
 * таймаут, сбор stdout/stderr, маппинг exit-кода в ошибку.
 *
 * Изолировано от `rfq-cli.ts` намеренно: handler / тесты могут подменить
 * `RfqExecutor` фейком и проверить контракт без cluttered child_process.
 */
import { spawn } from "node:child_process";
import type { BuiltRfqCommand, RfqCliConfig } from "./rfq-cli.js";

export type RfqRunResult = Readonly<{
  exitCode: number;
  stdout: string;
  stderr: string;
  /** Дублирует built.display — полезно для audit_log без хранения raw args. */
  command: string;
  timedOut: boolean;
}>;

export type RfqExecutor = (params: Readonly<{
  cfg: RfqCliConfig;
  built: BuiltRfqCommand;
  /** Опциональный AbortSignal для shutdown воркера. */
  signal?: AbortSignal;
}>) => Promise<RfqRunResult>;

/**
 * Прод-имплементация: spawn `built.cmd built.args`, без shell.
 * Никаких unbounded buffers: на каждой стороне держим максимум `MAX_BYTES`
 * для последних байт — RFQ-логи могут быть длинными.
 */
const MAX_BYTES = 1 * 1024 * 1024; // 1 MiB на поток

export const defaultRfqExecutor: RfqExecutor = ({ cfg, built, signal }) => {
  return new Promise<RfqRunResult>((resolve, reject) => {
    let child;
    try {
      child = spawn(built.cmd, [...built.args], {
        stdio: ["ignore", "pipe", "pipe"],
        env: { ...process.env },
      });
    } catch (e) {
      reject(e instanceof Error ? e : new Error(String(e)));
      return;
    }

    let stdoutBytes = 0;
    let stderrBytes = 0;
    const stdoutChunks: Buffer[] = [];
    const stderrChunks: Buffer[] = [];
    let timedOut = false;

    const onAbort = () => {
      if (!child.killed) child.kill("SIGTERM");
    };
    if (signal) {
      if (signal.aborted) onAbort();
      else signal.addEventListener("abort", onAbort, { once: true });
    }

    const timer = cfg.timeoutMs > 0
      ? setTimeout(() => {
          timedOut = true;
          if (!child.killed) child.kill("SIGTERM");
        }, cfg.timeoutMs)
      : null;

    child.stdout?.on("data", (chunk: Buffer) => {
      stdoutBytes += chunk.length;
      stdoutChunks.push(chunk);
      while (stdoutBytes > MAX_BYTES && stdoutChunks.length > 1) {
        const dropped = stdoutChunks.shift();
        if (dropped) stdoutBytes -= dropped.length;
      }
    });
    child.stderr?.on("data", (chunk: Buffer) => {
      stderrBytes += chunk.length;
      stderrChunks.push(chunk);
      while (stderrBytes > MAX_BYTES && stderrChunks.length > 1) {
        const dropped = stderrChunks.shift();
        if (dropped) stderrBytes -= dropped.length;
      }
    });

    child.on("error", (err) => {
      if (timer) clearTimeout(timer);
      if (signal) signal.removeEventListener("abort", onAbort);
      reject(err);
    });
    child.on("close", (code) => {
      if (timer) clearTimeout(timer);
      if (signal) signal.removeEventListener("abort", onAbort);
      resolve({
        exitCode: code ?? -1,
        stdout: Buffer.concat(stdoutChunks).toString("utf8"),
        stderr: Buffer.concat(stderrChunks).toString("utf8"),
        command: built.display,
        timedOut,
      });
    });
  });
};
