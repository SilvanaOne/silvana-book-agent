/**
 * One-shot bootstrap: ensure an RSA keypair exists and (optionally) seed an
 * initial operator account from env vars.
 *
 * Bootstrap operator is created only if `OPERATOR_USERNAME` and
 * `OPERATOR_PASSWORD_BOOTSTRAP` are set AND no operators exist yet. After
 * the first run those env vars should be removed; we log a warning each
 * time they are present alongside an existing operator.
 */

import { getDb } from '@arbitrage-agent/db';
import { createLogger } from '@arbitrage-agent/shared';
import { ensureKeypair } from './rsa.js';
import { hashPassword } from './password.js';

const log = createLogger('api.auth.bootstrap');

export async function bootstrapAuth(): Promise<void> {
  await ensureKeypair();

  const username = process.env['OPERATOR_USERNAME'];
  const password = process.env['OPERATOR_PASSWORD_BOOTSTRAP'];

  const operatorCount = await getDb().operator.count();
  if (operatorCount > 0) {
    if (username || password) {
      log.warn('OPERATOR_USERNAME/OPERATOR_PASSWORD_BOOTSTRAP set but operators already exist — ignoring (remove from env)');
    }
    return;
  }

  if (!username || !password) {
    log.warn('no operator exists and OPERATOR_USERNAME/OPERATOR_PASSWORD_BOOTSTRAP not set — UI login will fail until an operator is seeded');
    return;
  }

  const passwordHash = await hashPassword(password);
  const op = await getDb().operator.create({ data: { username, passwordHash } });
  log.info({ id: op.id, username }, 'bootstrap operator created');
}
