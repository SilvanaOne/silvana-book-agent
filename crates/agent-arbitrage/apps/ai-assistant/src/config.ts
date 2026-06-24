import type { ProviderName } from '@arbitrage-agent/ai-assistant-core';

export interface Config {
  readonly port: number;
  readonly apiToken: string;
  readonly maxSkewSec: number;
  readonly enabled: boolean;
  readonly provider: ProviderName;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): Config {
  const provider = (env['AI_PROVIDER'] ?? 'heuristic') as ProviderName;
  return {
    port: Number(env['AI_ASSISTANT_PORT'] ?? 4004),
    apiToken: env['AI_ASSISTANT_API_TOKEN'] ?? 'dev-ai-secret',
    maxSkewSec: Number(env['AI_ASSISTANT_MAX_SKEW_SEC'] ?? 300),
    // Opt-in kill-switch (md_docs/13 Q1). Default on — the module is advisory-only.
    enabled: (env['AI_ASSISTANT_ENABLED'] ?? '1') !== '0',
    provider,
  };
}
