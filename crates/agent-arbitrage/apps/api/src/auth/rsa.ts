/**
 * RSA-OAEP keypair for protecting secrets sent from the UI to the API
 * (currently: passwords during login; future: CEX/Canton credentials if we
 * ever add a UI for them — though the open-core dashboard MUST NOT hold
 * those, see md_docs/12-component-split).
 *
 * The keypair is generated once on first boot and persisted in the
 * `RsaKeypair` table (purpose='ui-session'). The public key is served by
 * `GET /api/auth/pubkey`; the UI uses WebCrypto RSA-OAEP-256 with this key.
 */

import { generateKeyPairSync, privateDecrypt, constants, createPrivateKey } from 'node:crypto';
import { getDb } from '@arbitrage-agent/db';
import { createLogger } from '@arbitrage-agent/shared';

const log = createLogger('api.auth.rsa');

const PURPOSE = 'ui-session';

export interface RsaKeyMaterial {
  readonly publicKeyPem: string;
  readonly privateKeyPem: string;
}

let cached: RsaKeyMaterial | null = null;

export async function ensureKeypair(): Promise<RsaKeyMaterial> {
  if (cached) return cached;

  const existing = await getDb().rsaKeypair.findFirst({
    where: { purpose: PURPOSE },
    orderBy: { createdAt: 'desc' },
  });
  if (existing) {
    cached = { publicKeyPem: existing.publicKeyPem, privateKeyPem: existing.privateKeyPem };
    log.info({ id: existing.id }, 'rsa keypair loaded');
    return cached;
  }

  const { publicKey, privateKey } = generateKeyPairSync('rsa', {
    modulusLength: 2048,
    publicKeyEncoding: { type: 'spki', format: 'pem' },
    privateKeyEncoding: { type: 'pkcs8', format: 'pem' },
  });
  const created = await getDb().rsaKeypair.create({
    data: { publicKeyPem: publicKey, privateKeyPem: privateKey, purpose: PURPOSE },
  });
  cached = { publicKeyPem: publicKey, privateKeyPem: privateKey };
  log.info({ id: created.id }, 'rsa keypair generated');
  return cached;
}

export async function getPublicKeyPem(): Promise<string> {
  const k = await ensureKeypair();
  return k.publicKeyPem;
}

/**
 * Decrypt a base64-encoded ciphertext produced by WebCrypto RSA-OAEP with
 * SHA-256. The browser sends ciphertext as a base64 string; we decrypt with
 * the stored private key and return UTF-8 plaintext.
 */
export async function decryptFromUi(ciphertextB64: string): Promise<string> {
  const k = await ensureKeypair();
  const key = createPrivateKey(k.privateKeyPem);
  const decrypted = privateDecrypt(
    { key, padding: constants.RSA_PKCS1_OAEP_PADDING, oaepHash: 'sha256' },
    Buffer.from(ciphertextB64, 'base64'),
  );
  return decrypted.toString('utf8');
}
