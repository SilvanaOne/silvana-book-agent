import { authHeaders } from './auth.js';
import { fetchJson } from '@arbitrage-agent/shared';
import { ExecutorClient } from './executor-client.js';
import type { EngineClientConfig } from './base.js';

export type ExecutorRuntimeStatus = 'ready' | 'provisioning' | 'sleeping' | 'unavailable';

export interface TenantRuntimeResponse {
  readonly tenantUserId: string;
  readonly keysReady: boolean;
  readonly executor: {
    readonly url: string;
    readonly apiToken: string;
    readonly status: ExecutorRuntimeStatus;
  } | null;
}

export interface VaultRuntimeConfig {
  readonly vaultUrl: string;
  readonly vaultToken: string;
  readonly tenantUserId: string;
  readonly timeoutMs?: number;
}

/** Agent first-start heartbeat — provisions managed executor via Silvana Vault. */
export async function postAgentHeartbeat(cfg: VaultRuntimeConfig): Promise<TenantRuntimeResponse> {
  const path = '/agent/heartbeat';
  const body = JSON.stringify({ tenantUserId: cfg.tenantUserId });
  return fetchJson<TenantRuntimeResponse>(`${cfg.vaultUrl.replace(/\/$/, '')}${path}`, {
    method: 'POST',
    body,
    headers: {
      'Content-Type': 'application/json',
      ...authHeaders(cfg.vaultToken, 'POST', path, body),
    },
    timeoutMs: cfg.timeoutMs ?? 30_000,
    maxRetries: 2,
  });
}

export function executorClientFromRuntime(runtime: TenantRuntimeResponse): ExecutorClient | null {
  const ex = runtime.executor;
  if (!ex?.url || !ex.apiToken) return null;
  if (ex.status === 'unavailable') return null;
  const clientCfg: EngineClientConfig = {
    baseUrl: ex.url.replace(/\/$/, ''),
    token: ex.apiToken,
  };
  return new ExecutorClient(clientCfg);
}

/**
 * Resolve executor client: static EXECUTOR_API_URL wins; else managed runtime via vault heartbeat.
 */
export async function resolveExecutorClient(
  env: Record<string, string | undefined> = process.env,
): Promise<{ client: ExecutorClient | null; runtime: TenantRuntimeResponse | null }> {
  const staticUrl = env['EXECUTOR_API_URL'];
  if (staticUrl) {
    const token = env['EXECUTOR_API_TOKEN'] ?? '';
    return { client: new ExecutorClient({ baseUrl: staticUrl.replace(/\/$/, ''), token }), runtime: null };
  }

  const tenantUserId = env['SIGNER_TENANT_USER_ID'];
  const vaultUrl = env['SIGNER_API_URL'];
  const vaultToken = env['SIGNER_API_TOKEN'];
  if (!tenantUserId || !vaultUrl || !vaultToken) {
    return { client: null, runtime: null };
  }

  const runtime = await postAgentHeartbeat({ vaultUrl, vaultToken, tenantUserId });
  return { client: executorClientFromRuntime(runtime), runtime };
}
