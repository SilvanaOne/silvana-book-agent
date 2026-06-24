/**
 * DB-backed runtime config with env fallback.
 *
 * Why two layers: env vars set the *initial* operating point at deploy time;
 * once an operator edits a field via the UI, the DB value takes over and
 * persists across restarts. Restoring defaults = DELETE the row.
 *
 * Kill switch (`runtime.killSwitch`) is read by the scanner each cycle.
 */

import { getDb } from '@arbitrage-agent/db';

export interface TradeConfigDto {
  readonly targetSpreadPercent: number;
  readonly maxSpreadPercent: number;
  readonly tradeSizeUsd: number;
}

export interface RiskConfigDto {
  readonly maxSpreadBps: number;
  readonly maxQuoteAgeMs: number;
  readonly dailyLossLimitUsd: number;
  readonly maxConsecutiveLosses: number;
}

export interface RuntimeDto {
  readonly strategyMode: string;
  readonly silvanaHostMode: string;
  readonly scanIntervalMs: number;
  readonly scannerPersist: boolean;
  readonly killSwitch: boolean;
}

export interface RuntimeConfigDto {
  readonly tradeConfig: TradeConfigDto;
  readonly riskConfig: RiskConfigDto;
  readonly runtime: RuntimeDto;
}

const KEYS = {
  tradeConfig: 'tradeConfig',
  riskConfig: 'riskConfig',
  killSwitch: 'runtime.killSwitch',
} as const;

function envDefaults(): RuntimeConfigDto {
  return {
    tradeConfig: {
      targetSpreadPercent: Number(process.env['TARGET_SPREAD_PERCENT'] ?? 0.25),
      maxSpreadPercent: Number(process.env['MAX_SPREAD_PERCENT'] ?? 10),
      tradeSizeUsd: Number(process.env['TRADE_SIZE_USD'] ?? 100),
    },
    riskConfig: {
      maxSpreadBps: Number(process.env['RISK_MAX_SPREAD_BPS'] ?? 500),
      maxQuoteAgeMs: Number(process.env['RISK_MAX_QUOTE_AGE_MS'] ?? 5000),
      dailyLossLimitUsd: Number(process.env['RISK_DAILY_LOSS_LIMIT_USD'] ?? 100),
      maxConsecutiveLosses: Number(process.env['RISK_MAX_CONSECUTIVE_LOSSES'] ?? 3),
    },
    runtime: {
      strategyMode: process.env['STRATEGY_MODE'] ?? 'paper',
      silvanaHostMode: process.env['SILVANA_HOST_MODE'] ?? 'standalone-dev',
      scanIntervalMs: Number(process.env['SCAN_INTERVAL_MS'] ?? 5000),
      scannerPersist: process.env['SCANNER_PERSIST'] === '1',
      killSwitch: false,
    },
  };
}

export async function getRuntimeConfig(): Promise<RuntimeConfigDto> {
  const base = envDefaults();
  const rows = await getDb().config.findMany({
    where: { key: { in: [KEYS.tradeConfig, KEYS.riskConfig, KEYS.killSwitch] } },
  });
  const map = new Map(rows.map((r) => [r.key, r.value]));

  const trade = map.get(KEYS.tradeConfig) as Partial<TradeConfigDto> | undefined;
  const risk = map.get(KEYS.riskConfig) as Partial<RiskConfigDto> | undefined;
  const kill = map.get(KEYS.killSwitch) as { enabled?: boolean } | undefined;

  return {
    tradeConfig: { ...base.tradeConfig, ...trade },
    riskConfig: { ...base.riskConfig, ...risk },
    runtime: { ...base.runtime, killSwitch: Boolean(kill?.enabled) },
  };
}

export async function updateTradeConfig(
  patch: Partial<TradeConfigDto>,
  actor: string,
): Promise<TradeConfigDto> {
  const current = (await getRuntimeConfig()).tradeConfig;
  const next: TradeConfigDto = { ...current, ...patch };
  await getDb().config.upsert({
    where: { key: KEYS.tradeConfig },
    create: { key: KEYS.tradeConfig, value: next as unknown as object, updatedBy: actor },
    update: { value: next as unknown as object, updatedBy: actor },
  });
  await getDb().auditLog.create({
    data: { actor, action: 'config.tradeConfig.update', payload: patch },
  });
  return next;
}

export async function updateRiskConfig(
  patch: Partial<RiskConfigDto>,
  actor: string,
): Promise<RiskConfigDto> {
  const current = (await getRuntimeConfig()).riskConfig;
  const next: RiskConfigDto = { ...current, ...patch };
  await getDb().config.upsert({
    where: { key: KEYS.riskConfig },
    create: { key: KEYS.riskConfig, value: next as unknown as object, updatedBy: actor },
    update: { value: next as unknown as object, updatedBy: actor },
  });
  await getDb().auditLog.create({
    data: { actor, action: 'config.riskConfig.update', payload: patch },
  });
  return next;
}

export async function setKillSwitch(enabled: boolean, actor: string): Promise<boolean> {
  await getDb().config.upsert({
    where: { key: KEYS.killSwitch },
    create: { key: KEYS.killSwitch, value: { enabled }, updatedBy: actor },
    update: { value: { enabled }, updatedBy: actor },
  });
  await getDb().auditLog.create({
    data: { actor, action: enabled ? 'killSwitch.engage' : 'killSwitch.release', payload: {} },
  });
  return enabled;
}

export async function getKillSwitch(): Promise<boolean> {
  const row = await getDb().config.findUnique({ where: { key: KEYS.killSwitch } });
  if (!row) return false;
  const v = row.value as { enabled?: boolean };
  return Boolean(v?.enabled);
}
