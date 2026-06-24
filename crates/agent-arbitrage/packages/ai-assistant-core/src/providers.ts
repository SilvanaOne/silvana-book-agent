/**
 * LLM provider abstraction (md_docs/13 Q3). Switch via config (AI_PROVIDER):
 * heuristic (default — deterministic, no key, no network), anthropic, openai,
 * ollama. The cloud/local providers are stubbed in this skeleton — they throw
 * until an SDK + key are wired (consistent with "keys added later").
 *
 * Feature modules use the provider only to phrase/explain; when the provider is
 * `heuristic` they run a deterministic local computation (no LLM), mirroring the
 * existing operator-facing /assistant heuristics.
 */

import type { ProviderName } from './types.js';

export interface Prompt {
  readonly system: string;
  readonly user: string;
}

export interface LlmProvider {
  readonly name: ProviderName;
  complete(prompt: Prompt): Promise<string>;
}

/** Cloud/local provider placeholder — real SDK call wired in once keys land. */
class StubProvider implements LlmProvider {
  constructor(readonly name: ProviderName) {}
  complete(_prompt: Prompt): Promise<string> {
    return Promise.reject(
      new Error(`AI provider '${this.name}' not wired in skeleton — set AI_PROVIDER + API key (or use heuristic)`),
    );
  }
}

/**
 * Returns the provider for real-LLM phrasing, or `null` for the heuristic path
 * (feature modules then compute deterministically — no LLM call).
 */
export function makeProvider(name: ProviderName): LlmProvider | null {
  switch (name) {
    case 'heuristic':
      return null;
    case 'anthropic':
    case 'openai':
    case 'ollama':
      return new StubProvider(name);
    default:
      return null;
  }
}
