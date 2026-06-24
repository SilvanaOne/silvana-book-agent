/**
 * @arbitrage-agent/clients — typed HTTP clients for the proprietary engines.
 *
 * OPEN CORE. These describe how to call the executor / rebalancer / signer over
 * HTTP (signed with HMAC) and parse their typed responses. They hold no keys and
 * contain no engine logic. The engines live in separate private repos; conform
 * to schemas/proprietary-engine-api.yaml. See md_docs/12 + md_docs/upgrade-plan.md.
 */

export * from './types.js';
export { authHeaders, signRequest, canonicalString, TIMESTAMP_HEADER, SIGNATURE_HEADER } from './auth.js';
export { BaseEngineClient, EngineClientError } from './base.js';
export type { EngineClientConfig } from './base.js';
export { ExecutorClient, toOpportunityDTO } from './executor-client.js';
export { RebalancerClient } from './rebalancer-client.js';
export { SignerClient } from './signer-client.js';
export { executorClientFromEnv, rebalancerClientFromEnv, signerClientFromEnv } from './env.js';
export {
  postAgentHeartbeat,
  resolveExecutorClient,
  executorClientFromRuntime,
  type TenantRuntimeResponse,
  type ExecutorRuntimeStatus,
  type VaultRuntimeConfig,
} from './tenant-runtime.js';
