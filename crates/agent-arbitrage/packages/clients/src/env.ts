/**
 * Factory helpers that build engine clients from environment variables.
 *
 * Returns `null` when the engine URL is not configured, so callers can degrade
 * gracefully (open core keeps running detection-only when engines are absent).
 */

import type { EngineClientConfig } from './base.js';
import { ExecutorClient } from './executor-client.js';
import { RebalancerClient } from './rebalancer-client.js';
import { SignerClient } from './signer-client.js';

type Env = Record<string, string | undefined>;

function configFrom(env: Env, urlKey: string, tokenKey: string): EngineClientConfig | null {
  const baseUrl = env[urlKey];
  if (!baseUrl) return null;
  return { baseUrl: baseUrl.replace(/\/$/, ''), token: env[tokenKey] ?? '' };
}

export function executorClientFromEnv(env: Env = process.env): ExecutorClient | null {
  const cfg = configFrom(env, 'EXECUTOR_API_URL', 'EXECUTOR_API_TOKEN');
  return cfg ? new ExecutorClient(cfg) : null;
}

export function rebalancerClientFromEnv(env: Env = process.env): RebalancerClient | null {
  const cfg = configFrom(env, 'REBALANCER_API_URL', 'REBALANCER_API_TOKEN');
  return cfg ? new RebalancerClient(cfg) : null;
}

export function signerClientFromEnv(env: Env = process.env): SignerClient | null {
  const cfg = configFrom(env, 'SIGNER_API_URL', 'SIGNER_API_TOKEN');
  return cfg ? new SignerClient(cfg) : null;
}
