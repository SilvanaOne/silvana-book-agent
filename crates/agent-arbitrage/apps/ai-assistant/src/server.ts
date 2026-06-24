/**
 * AI advisory microservice (md_docs/13). ADVISORY ONLY — suggests, the operator
 * confirms. No keys, no DB, no Telegram: callers pass the data (e.g. a trade
 * record) in the request; outputs are suggestions + pending approvals.
 *
 *   POST /ai/trade-postmortem        { trade }
 *   POST /ai/config-suggest          { naturalLanguage, currentConfig }
 *   POST /ai/resolve-symbol          { query, context? }
 *   POST /ai/approvals/:id/confirm
 *   POST /ai/approvals/:id/reject
 *   GET  /health
 */

import express, { type Express, type Request, type Response } from 'express';
import {
  ApprovalManager,
  CostGuard,
  configSuggest,
  makeProvider,
  resolveSymbol,
  tradePostmortem,
} from '@arbitrage-agent/ai-assistant-core';
import type { Config } from './config.js';
import { hmacAuth, type RawBodyRequest } from './auth.js';

export function createApp(cfg: Config): Express {
  const app = express();
  app.use(express.json({ verify: (req, _res, buf) => ((req as RawBodyRequest).rawBody = buf.toString('utf8')) }));

  const approvals = new ApprovalManager();
  const guard = new CostGuard();
  // Provider is instantiated for future real-LLM phrasing; heuristic → null.
  const provider = makeProvider(cfg.provider);

  app.get('/health', (_req, res) => {
    res.json({ status: 'ok', enabled: cfg.enabled, provider: cfg.provider, llm: provider ? 'real' : 'heuristic' });
  });

  // Kill-switch: when disabled, advisory endpoints 503 (md_docs/13 Q1).
  app.use('/ai', (_req, res, next) => {
    if (!cfg.enabled) {
      res.status(503).json({ error: 'ai_assistant_disabled' });
      return;
    }
    next();
  });

  app.use('/ai', hmacAuth(cfg));

  // Per-operator cost/rate guard before any advisory work.
  app.use('/ai', (req: Request, res: Response, next) => {
    const operatorId = req.header('x-operator-id') ?? 'default';
    const decision = guard.check(operatorId);
    if (!decision.ok) {
      res.status(429).json({ error: 'rate_or_cost_capped', detail: decision.reason });
      return;
    }
    next();
  });

  app.post('/ai/trade-postmortem', (req: Request, res: Response) => {
    if (!req.body?.trade) {
      res.status(400).json({ error: 'trade required' });
      return;
    }
    res.json(tradePostmortem(req.body.trade));
  });

  app.post('/ai/config-suggest', (req: Request, res: Response) => {
    const { naturalLanguage, currentConfig } = req.body ?? {};
    if (typeof naturalLanguage !== 'string') {
      res.status(400).json({ error: 'naturalLanguage required' });
      return;
    }
    res.json(configSuggest(naturalLanguage, currentConfig ?? {}, approvals));
  });

  app.post('/ai/resolve-symbol', (req: Request, res: Response) => {
    const { query, context } = req.body ?? {};
    if (typeof query !== 'string') {
      res.status(400).json({ error: 'query required' });
      return;
    }
    res.json(resolveSymbol(query, context));
  });

  app.post('/ai/approvals/:id/confirm', (req: Request, res: Response) => {
    const payload = approvals.confirm(String(req.params.id));
    if (payload === null) {
      res.status(409).json({ error: 'not_confirmable' });
      return;
    }
    res.json({ approved: true, payload });
  });

  app.post('/ai/approvals/:id/reject', (req: Request, res: Response) => {
    res.json({ rejected: approvals.reject(String(req.params.id)) });
  });

  return app;
}
