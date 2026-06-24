/**
 * Execution control plane (open-core gateway to the proprietary engines).
 *
 *   GET  /api/execution/status         — executor + rebalancer configured/reachable
 *   POST /api/execution/emergency-stop — proxy executor kill-switch
 *
 * The engines live in separate private repos; open core only signals. When an
 * engine URL is unset the UI shows execution as "disabled" and detection keeps
 * running (graceful degrade). See md_docs/upgrade-plan.md §U4.
 */

import { Router, type Request, type Response } from 'express';
import { createLogger } from '@arbitrage-agent/shared';
import { executorClientFromEnv, rebalancerClientFromEnv } from '@arbitrage-agent/clients';

const log = createLogger('api:execution');

export const executionRouter = Router();

executionRouter.get('/status', async (_req: Request, res: Response) => {
  const executor = executorClientFromEnv();
  const rebalancer = rebalancerClientFromEnv();
  const [executorReachable, rebalancerReachable] = await Promise.all([
    executor ? executor.isHealthy() : Promise.resolve(false),
    rebalancer ? rebalancer.isHealthy() : Promise.resolve(false),
  ]);
  const strategyMode = process.env['STRATEGY_MODE'] ?? 'paper';
  const executionEnabled = strategyMode !== 'paper';
  res.json({
    strategyMode,
    executionEnabled,
    executor: { configured: executor !== null, reachable: executorReachable },
    rebalancer: { configured: rebalancer !== null, reachable: rebalancerReachable },
  });
});

executionRouter.post('/emergency-stop', async (_req: Request, res: Response) => {
  const executor = executorClientFromEnv();
  if (!executor) {
    res.status(503).json({ error: 'executor_not_configured' });
    return;
  }
  try {
    const result = await executor.emergencyStop();
    res.json(result);
  } catch (err: unknown) {
    log.warn({ err: err instanceof Error ? err.message : String(err) }, 'emergency-stop failed');
    res.status(502).json({ error: 'executor_unreachable' });
  }
});
