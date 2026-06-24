/**
 * Scanner — main long-running process.
 *
 * Sprint 1 status: real spread detection loop for Bybit ↔ KuCoin CC/USDT.
 * Periodic poll via REST. Detected opportunities logged via Pino and
 * (optionally) persisted to Postgres.
 *
 * In `STRATEGY_MODE=paper` (default) opportunities are NOT forwarded to the
 * proprietary executor — purely observational. Live execution wiring lands
 * once the proprietary executor service is reachable.
 *
 * See md_docs/04-architecture-blueprint.md §"apps/scanner" + md_docs/09 Sprint 1.
 */

import 'dotenv/config';
import { createSilvanaHost, type SilvanaHostMode } from '@arbitrage-agent/silvana-host';
import {
  createToken,
  createPair,
  createLogger,
  tokenPairKey,
  type Pair,
  type IVenueProvider,
  type SpreadOpportunity,
} from '@arbitrage-agent/shared';
import {
  ExecutorClient,
  executorClientFromEnv,
  rebalancerClientFromEnv,
  resolveExecutorClient,
  postAgentHeartbeat,
  toOpportunityDTO,
  type DualTradeStatus,
} from '@arbitrage-agent/clients';
import { runSpreadScanCycle, type TradeConfig } from '@arbitrage-agent/strategy';
import {
  evaluate as evaluateRisk,
  createInitialState as createRiskState,
  type RiskConfig,
  type RiskState,
} from '@arbitrage-agent/risk';
import { BybitProvider } from '@arbitrage-agent/bybit-client';
import { KucoinProvider } from '@arbitrage-agent/kucoin-client';
import { OneSwapProvider, type OneSwapMode, type OneSwapEnvironment } from '@arbitrage-agent/oneswap-client';
import { TempleProvider, type TempleMode, type TempleNetwork } from '@arbitrage-agent/temple-client';
import { CantexProvider, type CantexMode, type CantexNetwork } from '@arbitrage-agent/cantex-client';
import { SilvanaProvider, type SilvanaMode } from '@arbitrage-agent/silvana-client';
import {
  initPersistence,
  shutdownPersistence,
  ensureSeed,
  recordSpread,
  getKillSwitch,
  recordVenueHealth,
  recordTradeExecution,
  recordInventory,
  type PersistenceContext,
  type VenueOutcome,
} from './persistence.js';

const log = createLogger('scanner');

const SILVANA_HOST_MODE = (process.env['SILVANA_HOST_MODE'] ?? 'standalone-dev') as SilvanaHostMode;
const SCAN_INTERVAL_MS = Number(process.env['SCAN_INTERVAL_MS'] ?? 5000);
const STRATEGY_MODE = process.env['STRATEGY_MODE'] ?? 'paper';
const TARGET_SPREAD_PERCENT = Number(process.env['TARGET_SPREAD_PERCENT'] ?? 0.25);
const MAX_SPREAD_PERCENT = Number(process.env['MAX_SPREAD_PERCENT'] ?? 10);
const TRADE_SIZE_USD = Number(process.env['TRADE_SIZE_USD'] ?? 100);
const SCANNER_PERSIST = process.env['SCANNER_PERSIST'] === '1';

const RISK_MAX_SPREAD_BPS = Number(process.env['RISK_MAX_SPREAD_BPS'] ?? 500);
const RISK_MAX_QUOTE_AGE_MS = Number(process.env['RISK_MAX_QUOTE_AGE_MS'] ?? 5000);
const RISK_DAILY_LOSS_LIMIT_USD = Number(process.env['RISK_DAILY_LOSS_LIMIT_USD'] ?? 100);
const RISK_MAX_CONSECUTIVE_LOSSES = Number(process.env['RISK_MAX_CONSECUTIVE_LOSSES'] ?? 3);

// Sprint 3: OneSwap (Canton AMM) added as M3 read-client. Enabled by default
// in mock mode (no network / credentials). Set ONESWAP_MODE=live + ONESWAP_API_KEY
// for real devnet quotes. Cross-venue OneSwap spreads activate once a venue
// sharing its pairs lands (Temple USDCx cluster, Sprint 4).
const ONESWAP_ENABLED = (process.env['ONESWAP_ENABLED'] ?? '1') === '1';
const ONESWAP_MODE = (process.env['ONESWAP_MODE'] ?? 'mock') as OneSwapMode;
const ONESWAP_ENV = (process.env['ONESWAP_ENV'] ?? 'devnet') as OneSwapEnvironment;
const ONESWAP_API_KEY = process.env['ONESWAP_API_KEY'];

// Sprint 4: Temple (Canton CLOB). Shares CC/USDCx with OneSwap → first
// cross-venue Canton spread (M3↔M4, USDCx cluster). Mock by default.
const TEMPLE_ENABLED = (process.env['TEMPLE_ENABLED'] ?? '1') === '1';
const TEMPLE_MODE = (process.env['TEMPLE_MODE'] ?? 'mock') as TempleMode;
const TEMPLE_NETWORK = (process.env['TEMPLE_NETWORK'] ?? 'testnet') as TempleNetwork;
const TEMPLE_API_KEY = process.env['TEMPLE_API_KEY'];

// Sprint 5: Cantex (Canton AMM). Shares CC/USDCx with OneSwap + Temple →
// completes the 3-way USDCx cluster (M3↔M4↔M5). Mock by default.
const CANTEX_ENABLED = (process.env['CANTEX_ENABLED'] ?? '1') === '1';
const CANTEX_MODE = (process.env['CANTEX_MODE'] ?? 'mock') as CantexMode;
const CANTEX_NETWORK = (process.env['CANTEX_NETWORK'] ?? 'mainnet') as CantexNetwork;
const CANTEX_API_KEY = process.env['CANTEX_API_KEY'];

// Sprint 6: Silvana Book (Canton CLOB) read-only quote feed. CC/USDC — a
// separate stable cluster from the USDCx venues. Mock by default; live gRPC
// pricing is deferred until onboarding. The USDC↔USDCx cross-cluster spread
// is detected by the Sprint 7 CC-triangulation analyzer.
const SILVANA_ENABLED = (process.env['SILVANA_ENABLED'] ?? '1') === '1';
const SILVANA_MODE = (process.env['SILVANA_MODE'] ?? 'mock') as SilvanaMode;

// Sprint 7: cross-cluster CC-triangulation detection (USDCx ↔ USDC clusters),
// net of stablecoin conversion cost. Detection only — execution + stable-router
// live in the proprietary executor/rebalancer.
const CROSS_CLUSTER_ENABLED = (process.env['CROSS_CLUSTER_ENABLED'] ?? '1') === '1';

// U4: forward accepted opportunities to the proprietary executor when
// STRATEGY_MODE != paper. Clients are null when the engine URL is unset →
// detection keeps running (graceful degrade). Execution/rebalancing logic
// itself lives in the proprietary engines; open core only signals + records.
const EXECUTION_ENABLED = STRATEGY_MODE !== 'paper';
const EXECUTOR_MAX_SLIPPAGE_BPS = Number(process.env['EXECUTOR_MAX_SLIPPAGE_BPS'] ?? 20);
const EXECUTOR_SAFETY_BUFFER_BPS = Number(process.env['EXECUTOR_SAFETY_BUFFER_BPS'] ?? 5);
const SKEW_POLL_CYCLES = Number(process.env['SKEW_POLL_CYCLES'] ?? 12);
const AGENT_HEARTBEAT_MS = Number(process.env['AGENT_HEARTBEAT_MS'] ?? 120_000);
const SIGNER_TENANT_USER_ID = process.env['SIGNER_TENANT_USER_ID'];
const SIGNER_API_URL = process.env['SIGNER_API_URL'];
const SIGNER_API_TOKEN = process.env['SIGNER_API_TOKEN'];

let executor: ExecutorClient | null = EXECUTION_ENABLED ? executorClientFromEnv() : null;
const rebalancer = EXECUTION_ENABLED ? rebalancerClientFromEnv() : null;

async function bootstrapManagedExecutor(): Promise<void> {
  if (!EXECUTION_ENABLED) return;
  if (process.env['EXECUTOR_API_URL']) {
    executor = executorClientFromEnv();
    return;
  }
  if (!SIGNER_TENANT_USER_ID || !SIGNER_API_URL || !SIGNER_API_TOKEN) {
    executor = null;
    return;
  }
  try {
    const { client, runtime } = await resolveExecutorClient(process.env);
    executor = client;
    if (runtime?.executor) {
      log.info(
        {
          tenantUserId: runtime.tenantUserId,
          executorUrl: runtime.executor.url,
          executorStatus: runtime.executor.status,
        },
        'managed executor resolved via vault heartbeat',
      );
    }
  } catch (err: unknown) {
    executor = null;
    log.warn(
      { err: err instanceof Error ? err.message : String(err) },
      'managed executor heartbeat failed — detection only until keys loaded / platform ready',
    );
  }
}

async function sendAgentHeartbeat(): Promise<void> {
  if (!EXECUTION_ENABLED || process.env['EXECUTOR_API_URL']) return;
  if (!SIGNER_TENANT_USER_ID || !SIGNER_API_URL || !SIGNER_API_TOKEN) return;
  try {
    const runtime = await postAgentHeartbeat({
      vaultUrl: SIGNER_API_URL,
      vaultToken: SIGNER_API_TOKEN,
      tenantUserId: SIGNER_TENANT_USER_ID,
    });
    if (runtime.executor?.url && runtime.executor.apiToken) {
      executor = new ExecutorClient({
        baseUrl: runtime.executor.url.replace(/\/$/, ''),
        token: runtime.executor.apiToken,
      });
    }
  } catch (err: unknown) {
    log.debug({ err: err instanceof Error ? err.message : String(err) }, 'periodic agent heartbeat failed');
  }
}
const TERMINAL_STATES = new Set(['executed', 'partial', 'failed', 'skipped', 'aborted']);
let executionDisabledLogged = false;
let scanCycleCount = 0;

function buildPairs(): readonly Pair[] {
  // Sprint 1: CC/USDT on Bybit + KuCoin. Sprint 3: + OneSwap CC/USDCx, CC/CBTC.
  // Sprint 2+: eventually load from DB Config / venue Markets table.
  const cc = createToken('CC', 'Canton Coin', 6, 'canton');
  const usdt = createToken('USDT', 'Tether USD', 6, 'cex');
  const pairs: Pair[] = [createPair(cc, usdt, 'bybit'), createPair(cc, usdt, 'kucoin')];

  const usdcx = createToken('USDCx', 'USDC (Canton)', 6, 'canton');
  const cbtc = createToken('CBTC', 'Canton BTC', 8, 'canton');

  if (ONESWAP_ENABLED) {
    pairs.push(createPair(cc, usdcx, 'oneswap'), createPair(cc, cbtc, 'oneswap'));
  }
  if (TEMPLE_ENABLED) {
    // CC/USDCx overlaps OneSwap (cross-venue spread); CBTC/USDCx is Temple-only for now.
    pairs.push(createPair(cc, usdcx, 'temple'), createPair(cbtc, usdcx, 'temple'));
  }
  if (CANTEX_ENABLED) {
    // CC/USDCx completes the 3-way USDCx cluster (OneSwap + Temple + Cantex).
    pairs.push(createPair(cc, usdcx, 'cantex'));
  }
  if (SILVANA_ENABLED) {
    // CC/USDC — the USDC cluster. Cross-cluster spread vs USDCx is Sprint 7.
    const usdc = createToken('USDC', 'USD Coin (Splice)', 6, 'canton');
    pairs.push(createPair(cc, usdc, 'silvana'));
  }
  return pairs;
}

function buildProviders(): readonly IVenueProvider[] {
  const providers: IVenueProvider[] = [new BybitProvider(), new KucoinProvider()];
  if (ONESWAP_ENABLED) {
    providers.push(
      new OneSwapProvider({
        mode: ONESWAP_MODE,
        environment: ONESWAP_ENV,
        ...(ONESWAP_API_KEY ? { apiKey: ONESWAP_API_KEY } : {}),
      }),
    );
  }
  if (TEMPLE_ENABLED) {
    providers.push(
      new TempleProvider({
        mode: TEMPLE_MODE,
        network: TEMPLE_NETWORK,
        ...(TEMPLE_API_KEY ? { apiKey: TEMPLE_API_KEY } : {}),
      }),
    );
  }
  if (CANTEX_ENABLED) {
    providers.push(
      new CantexProvider({
        mode: CANTEX_MODE,
        network: CANTEX_NETWORK,
        ...(CANTEX_API_KEY ? { apiKey: CANTEX_API_KEY } : {}),
      }),
    );
  }
  if (SILVANA_ENABLED) {
    providers.push(new SilvanaProvider({ mode: SILVANA_MODE }));
  }
  return providers;
}

function buildTradeConfig(): TradeConfig {
  return {
    targetSpreadPercent: TARGET_SPREAD_PERCENT,
    maxSpreadPercent: MAX_SPREAD_PERCENT,
    tradeSizeUsd: TRADE_SIZE_USD,
  };
}

function buildRiskConfig(): RiskConfig {
  return {
    maxSpreadBps: RISK_MAX_SPREAD_BPS,
    maxQuoteAgeMs: RISK_MAX_QUOTE_AGE_MS,
    dailyLossLimitUsd: RISK_DAILY_LOSS_LIMIT_USD,
    maxConsecutiveLosses: RISK_MAX_CONSECUTIVE_LOSSES,
  };
}

/** Poll the executor until the trade reaches a terminal state (or budget runs out). */
async function pollExecutorUntilTerminal(tradeId: string): Promise<DualTradeStatus | null> {
  if (!executor) return null;
  for (let i = 0; i < 15; i++) {
    await new Promise((r) => setTimeout(r, 300));
    try {
      const s = await executor.getDualTrade(tradeId);
      if (TERMINAL_STATES.has(s.status)) return s;
    } catch (err: unknown) {
      log.warn({ err: err instanceof Error ? err.message : String(err), tradeId }, 'executor poll failed');
      return null;
    }
  }
  return null;
}

/**
 * Forward an accepted opportunity to the proprietary executor, poll for the
 * result, and persist the per-leg TradeExecution. Never throws — a missing or
 * unreachable executor degrades to detection-only.
 */
async function forwardToExecutor(opp: SpreadOpportunity, persistence: PersistenceContext | null): Promise<void> {
  if (!executor) {
    if (!executionDisabledLogged) {
      log.warn('STRATEGY_MODE != paper but EXECUTOR_API_URL not set — execution disabled, detection only');
      executionDisabledLogged = true;
    }
    return;
  }
  try {
    const accepted = await executor.executeDualTrade({
      opportunity: toOpportunityDTO(opp),
      maxSlippageBps: EXECUTOR_MAX_SLIPPAGE_BPS,
      safetyBufferBps: EXECUTOR_SAFETY_BUFFER_BPS,
      dryRun: false,
      idempotencyKey: `${tokenPairKey(opp.pair)}-${opp.buyFrom.venueId}-${opp.sellTo.venueId}-${opp.detectedAt}`,
    });
    const final = await pollExecutorUntilTerminal(accepted.tradeId);
    if (final) {
      log.info(
        { tradeId: final.tradeId, status: final.status, profitUsd: final.profit?.usdtDiff, partial: final.partialExecution },
        'executor result',
      );
      if (persistence) await recordTradeExecution(persistence, final, opp);
    } else {
      log.warn({ tradeId: accepted.tradeId }, 'executor did not settle within poll budget');
    }
  } catch (err: unknown) {
    log.warn({ err: err instanceof Error ? err.message : String(err) }, 'executor forward failed — detection continues');
  }
}

/** Periodically pull inventory from the rebalancer and persist snapshots. */
async function pollSkewAndInventory(persistence: PersistenceContext | null): Promise<void> {
  if (!rebalancer || !persistence) return;
  try {
    const inv = await rebalancer.getInventory();
    await recordInventory(persistence, inv.snapshots);
    log.info({ snapshots: inv.snapshots.length }, 'inventory snapshot persisted');
  } catch (err: unknown) {
    log.warn({ err: err instanceof Error ? err.message : String(err) }, 'rebalancer inventory poll failed');
  }
}

async function scanOnce(
  pairs: readonly Pair[],
  providers: readonly IVenueProvider[],
  config: TradeConfig,
  riskConfig: RiskConfig,
  riskState: RiskState,
  persistence: PersistenceContext | null,
): Promise<void> {
  const t0 = Date.now();
  const { quoteMap, opportunities, fetchErrors } = await runSpreadScanCycle({
    pairs,
    providers,
    config,
    crossCluster: CROSS_CLUSTER_ENABLED,
  });
  const tookMs = Date.now() - t0;

  const quoteCount = Array.from(quoteMap.values()).reduce((sum, qs) => sum + qs.length, 0);

  for (const err of fetchErrors) {
    log.warn({ err }, 'fetch error');
  }

  if (persistence) {
    const succeeded = new Set<string>();
    for (const quotes of quoteMap.values()) {
      for (const q of quotes) succeeded.add(q.venueId);
    }
    const outcomes: VenueOutcome[] = providers.map((p) => {
      if (succeeded.has(p.venueId)) return { venueId: p.venueId, ok: true };
      // Errors are formatted as `${provider.name}: ${msg}` upstream.
      const err = fetchErrors.find((e) => e.startsWith(`${p.name}:`));
      return { venueId: p.venueId, ok: false, error: err ?? 'no quote' };
    });
    try {
      await recordVenueHealth(persistence, outcomes);
    } catch (err: unknown) {
      log.warn({ err: err instanceof Error ? err.message : String(err) }, 'venue health write failed');
    }
  }

  if (opportunities.length === 0) {
    log.info({ quotes: quoteCount, tookMs }, 'scan ok — 0 opportunities');
    return;
  }

  for (const opp of opportunities) {
    const pair = `${opp.pair.base.symbol}/${opp.pair.quote.symbol}`;
    const verdict = evaluateRisk(opp, riskState, riskConfig);
    const dbId = persistence ? await recordSpread(persistence, opp) : null;

    if (!verdict.ok) {
      log.info(
        {
          pair,
          buy: opp.buyFrom.venueId,
          sell: opp.sellTo.venueId,
          spreadPct: Number(opp.spreadPct.toFixed(4)),
          reason: verdict.reason,
          dbId: dbId !== null ? dbId.toString() : null,
        },
        'opportunity REJECTED by risk',
      );
      continue;
    }

    log.info(
      {
        pair,
        buy: opp.buyFrom.venueId,
        sell: opp.sellTo.venueId,
        spreadPct: Number(opp.spreadPct.toFixed(4)),
        buyPrice: opp.buyFrom.buyPrice,
        sellPrice: opp.sellTo.sellPrice,
        estProfitUsd: Number(opp.estimatedProfitUsd.toFixed(4)),
        dbId: dbId !== null ? dbId.toString() : null,
      },
      'opportunity accepted',
    );

    // U4: forward to the proprietary executor (when STRATEGY_MODE != paper).
    if (EXECUTION_ENABLED) {
      await forwardToExecutor(opp, persistence);
    }
  }

  // U4: periodic inventory/skew pull from the rebalancer.
  scanCycleCount += 1;
  if (EXECUTION_ENABLED && scanCycleCount % SKEW_POLL_CYCLES === 1) {
    await pollSkewAndInventory(persistence);
  }
}

async function main(): Promise<void> {
  log.info(
    {
      silvanaHostMode: SILVANA_HOST_MODE,
      strategyMode: STRATEGY_MODE,
      scanIntervalMs: SCAN_INTERVAL_MS,
      targetSpreadPercent: TARGET_SPREAD_PERCENT,
      persist: SCANNER_PERSIST,
    },
    'scanner starting',
  );

  const host = await createSilvanaHost({ mode: SILVANA_HOST_MODE });
  await host.start();
  log.info({ partyId: host.partyId, publicKey: host.publicKey }, 'silvana host ready');

  const pairs = buildPairs();
  const providers = buildProviders();
  const config = buildTradeConfig();
  const riskConfig = buildRiskConfig();
  const riskState: RiskState = createRiskState();
  log.info(
    { pairs: pairs.length, venues: providers.length, riskConfig },
    'scan plan',
  );

  await bootstrapManagedExecutor();

  let persistence: PersistenceContext | null = null;
  if (SCANNER_PERSIST) {
    try {
      persistence = await initPersistence();
      await ensureSeed(persistence, pairs);
      log.info('persistence ready — venues + instruments + markets seeded');
    } catch (err: unknown) {
      log.warn(
        { err: err instanceof Error ? err.message : String(err) },
        'persistence init failed — continuing in-memory',
      );
      persistence = null;
    }
  }

  let stopped = false;
  const shutdown = async (signal: string): Promise<void> => {
    if (stopped) return;
    stopped = true;
    log.info({ signal }, 'shutting down');
    if (persistence) await shutdownPersistence(persistence);
    await host.stop();
    process.exit(0);
  };
  process.on('SIGINT', () => void shutdown('SIGINT'));
  process.on('SIGTERM', () => void shutdown('SIGTERM'));

  if (EXECUTION_ENABLED && AGENT_HEARTBEAT_MS > 0) {
    setInterval(() => {
      void sendAgentHeartbeat();
    }, AGENT_HEARTBEAT_MS);
  }

  // Scan loop. We re-arm with setTimeout (not setInterval) so a slow cycle
  // never queues overlapping scans.
  let killSwitchLogged = false;
  const loop = async (): Promise<void> => {
    while (!stopped) {
      try {
        let killed = false;
        if (persistence) {
          try {
            killed = await getKillSwitch(persistence);
          } catch (err: unknown) {
            log.warn(
              { err: err instanceof Error ? err.message : String(err) },
              'kill-switch read failed',
            );
          }
        }
        if (killed) {
          if (!killSwitchLogged) {
            log.warn('kill switch engaged — scanning paused');
            killSwitchLogged = true;
          }
        } else {
          if (killSwitchLogged) {
            log.info('kill switch released — scanning resumed');
            killSwitchLogged = false;
          }
          await scanOnce(pairs, providers, config, riskConfig, riskState, persistence);
        }
      } catch (err: unknown) {
        log.error({ err: err instanceof Error ? err.message : String(err) }, 'scan error');
      }
      await new Promise((r) => setTimeout(r, SCAN_INTERVAL_MS));
    }
  };
  void loop();
}

main().catch((err: unknown) => {
  log.fatal({ err: err instanceof Error ? err.message : String(err) }, 'fatal');
  process.exit(1);
});
