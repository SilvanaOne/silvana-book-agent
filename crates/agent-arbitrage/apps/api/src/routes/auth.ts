/**
 * Auth routes — public endpoints (mounted before requireAuth).
 *
 * Login flow:
 *   1. UI calls GET /api/auth/pubkey → receives RSA public key PEM
 *   2. UI generates `encryptedPassword = RSA-OAEP-SHA256(publicKey, password)`
 *      (base64-encoded) and POSTs `{ username, encryptedPassword }`
 *   3. API decrypts, verifies hash, returns `{ token, expiresAt, operator }`
 *   4. UI stores token in localStorage and sends as `Authorization: Bearer ...`
 */

import { Router, type Request, type Response } from 'express';
import { getDb } from '@arbitrage-agent/db';
import { createLogger } from '@arbitrage-agent/shared';
import { getPublicKeyPem, decryptFromUi } from '../auth/rsa.js';
import { verifyPassword } from '../auth/password.js';
import { signToken } from '../auth/token.js';
import { requireAuth } from '../auth/middleware.js';

const log = createLogger('api.auth');

export const authRouter = Router();

authRouter.get('/pubkey', async (_req: Request, res: Response) => {
  try {
    const pem = await getPublicKeyPem();
    res.json({ publicKeyPem: pem });
  } catch (err: unknown) {
    log.error({ err: err instanceof Error ? err.message : String(err) }, 'pubkey failed');
    res.status(500).json({ error: 'internal_error' });
  }
});

authRouter.post('/login', async (req: Request, res: Response) => {
  const body = (req.body ?? {}) as { username?: unknown; encryptedPassword?: unknown };
  if (typeof body.username !== 'string' || typeof body.encryptedPassword !== 'string') {
    res.status(400).json({ error: 'bad_request' });
    return;
  }

  let password: string;
  try {
    password = await decryptFromUi(body.encryptedPassword);
  } catch {
    res.status(400).json({ error: 'bad_ciphertext' });
    return;
  }

  const op = await getDb().operator.findUnique({ where: { username: body.username } });
  if (!op || !op.enabled) {
    res.status(401).json({ error: 'invalid_credentials' });
    return;
  }

  const ok = await verifyPassword(password, op.passwordHash);
  if (!ok) {
    res.status(401).json({ error: 'invalid_credentials' });
    return;
  }

  const { token, expiresAt } = signToken({ sub: op.id, username: op.username });
  log.info({ username: op.username }, 'login ok');
  res.json({
    token,
    expiresAt,
    operator: { id: op.id, username: op.username, preferredLanguage: op.preferredLanguage },
  });
});

authRouter.get('/me', requireAuth, (req: Request, res: Response) => {
  res.json({ operator: req.operator });
});
