import { BaseEngineClient } from './base.js';
import type {
  Health,
  BalanceSnapshot,
  PublicKeyResponse,
  RuntimeBundleRequest,
  RuntimeBundleResponse,
  SaveCexSecretRequest,
  SaveWalletSecretRequest,
  SecretsStatusResponse,
} from './types.js';

/**
 * Typed client for the proprietary SIGNING VAULT (`arbitrage-signer`).
 *
 * Single source of CEX API keys + EVM/seed wallet keys. Model (B) key
 * distribution: engines fetch a scoped runtime bundle at boot and sign locally;
 * the vault is not in the per-order hot path. Canton Ed25519 identity is NOT
 * here — it lives in cloud-agent / Silvana host (md_docs/14).
 *
 * Open core uses only the provisioning surface (pubkey + save ciphertext +
 * status) to back the Wallet/Keys Manager UI. `/runtime/bundle` is called by
 * the engines, not open core.
 */
export class SignerClient extends BaseEngineClient {
  protected readonly engineName = 'signer';

  /** RSA public key (PEM) for browser-side secret encryption. */
  getPublicKey(): Promise<PublicKeyResponse> {
    return this.request<PublicKeyResponse>('GET', '/pubkey');
  }

  /** Store an RSA-encrypted CEX API key set (ciphertext only — encrypted in the browser). */
  saveCexSecret(req: SaveCexSecretRequest): Promise<void> {
    return this.request<void>('POST', '/secrets/cex', req);
  }

  /** Store an RSA-encrypted wallet private key or BIP39 seed (ciphertext only). */
  saveWalletSecret(req: SaveWalletSecretRequest): Promise<void> {
    return this.request<void>('POST', '/secrets/wallet', req);
  }

  /** Masked list of which secret slots are filled (no secret values). */
  getSecretsStatus(): Promise<SecretsStatusResponse> {
    return this.request<SecretsStatusResponse>('GET', '/secrets/status');
  }

  /** Read-only balance snapshot for tenant keys (CEX live, Canton placeholder). */
  getBalances(tenantUserId: string, force = false): Promise<BalanceSnapshot> {
    const q = new URLSearchParams({ tenantUserId });
    if (force) q.set('force', '1');
    return this.request<BalanceSnapshot>('GET', `/balances?${q}`);
  }

  /**
   * Scoped decrypted secrets for an engine (model B). Called by the engines on
   * boot/rotation — included here for completeness; open core does not call it.
   */
  getRuntimeBundle(req: RuntimeBundleRequest): Promise<RuntimeBundleResponse> {
    return this.request<RuntimeBundleResponse>('POST', '/runtime/bundle', req);
  }

  health(): Promise<Health> {
    return this.request<Health>('GET', '/health');
  }
}
