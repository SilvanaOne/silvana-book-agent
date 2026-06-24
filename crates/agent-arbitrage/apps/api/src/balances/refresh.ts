import { createLogger } from '@arbitrage-agent/shared';
import { signerClientFromEnv } from '@arbitrage-agent/clients';
import { setCachedBalances } from './cache.js';

const log = createLogger('api:balances');
const REFRESH_MS = Number(process.env['BALANCE_REFRESH_MS'] ?? 30_000);

export async function refreshBalancesFromVault(): Promise<void> {
  const tenantUserId = process.env['SIGNER_TENANT_USER_ID'];
  const signer = signerClientFromEnv();
  if (!tenantUserId || !signer) {
    log.debug('balances refresh skipped — SIGNER_TENANT_USER_ID or SIGNER_API_URL not set');
    return;
  }
  try {
    const snap = await signer.getBalances(tenantUserId, true);
    setCachedBalances(snap);
    log.debug({ sources: snap.sources.length, totalUsd: snap.totalUsd }, 'balances cache updated');
  } catch (err) {
    log.warn({ err: err instanceof Error ? err.message : String(err) }, 'balances refresh failed');
  }
}

export function startBalanceRefreshLoop(): void {
  void refreshBalancesFromVault();
  setInterval(() => void refreshBalancesFromVault(), REFRESH_MS);
  log.info({ refreshMs: REFRESH_MS }, 'balance refresh loop started');
}
