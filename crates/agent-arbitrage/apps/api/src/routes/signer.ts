/**
 * Signer proxy — open-core gateway to the proprietary signing vault.
 *
 * The browser (Wallet/Keys Manager UI) fetches the vault RSA public key, encrypts
 * each secret IN THE BROWSER, and POSTs only ciphertext. This server relays those
 * calls to the vault via the typed SignerClient, which signs each request with the
 * SIGNER_API_TOKEN HMAC secret. The token never reaches the browser, and open core
 * never sees plaintext secrets (it only forwards ciphertext + masked status).
 *
 * If SIGNER_API_URL is unset the vault is "not configured" → 503, and the UI shows
 * the feature as unavailable (graceful degrade). See md_docs/upgrade-plan.md §U1.
 */

import { Router, type Request, type Response } from 'express';
import { createLogger } from '@arbitrage-agent/shared';
import { signerClientFromEnv } from '@arbitrage-agent/clients';

const log = createLogger('api:signer');

export const signerRouter = Router();

// Built per request so it always reflects current env (and stays robust to
// import-order); construction is just an env read + object literal.
const getSigner = () => signerClientFromEnv();

async function proxy<T>(res: Response, label: string, fn: (s: NonNullable<ReturnType<typeof getSigner>>) => Promise<T>): Promise<void> {
  const signer = getSigner();
  if (!signer) {
    res.status(503).json({ error: 'signer_not_configured', detail: 'SIGNER_API_URL is not set' });
    return;
  }
  try {
    const result = await fn(signer);
    if (result === undefined) {
      res.status(204).end();
    } else {
      res.json(result);
    }
  } catch (err: unknown) {
    log.warn({ label, err: err instanceof Error ? err.message : String(err) }, 'signer proxy error');
    res.status(502).json({ error: 'signer_unreachable', detail: label });
  }
}

signerRouter.get('/pubkey', (_req: Request, res: Response) => proxy(res, 'pubkey', (s) => s.getPublicKey()));

signerRouter.get('/status', (_req: Request, res: Response) => proxy(res, 'status', (s) => s.getSecretsStatus()));

signerRouter.post('/secrets/cex', (req: Request, res: Response) =>
  proxy(res, 'secrets/cex', (s) => s.saveCexSecret(req.body)),
);

signerRouter.post('/secrets/wallet', (req: Request, res: Response) =>
  proxy(res, 'secrets/wallet', (s) => s.saveWalletSecret(req.body)),
);

signerRouter.get('/health', async (_req: Request, res: Response) => {
  const signer = getSigner();
  if (!signer) {
    res.json({ configured: false, reachable: false });
    return;
  }
  res.json({ configured: true, reachable: await signer.isHealthy() });
});
