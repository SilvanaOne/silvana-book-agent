/**
 * Password hashing with scrypt (Node built-in — no native dep).
 *
 * Stored format: `scrypt$N=<N>$r=<r>$p=<p>$<salt_b64>$<hash_b64>`. Parameters
 * are persisted alongside the hash so we can tune them later without breaking
 * existing operators.
 */

import { scrypt as scryptCb, randomBytes, timingSafeEqual } from 'node:crypto';

const N = 16384;
const r = 8;
const p = 1;
const KEY_LEN = 64;
const SALT_LEN = 16;

function scryptAsync(
  password: string,
  salt: Buffer,
  keylen: number,
  opts: { N: number; r: number; p: number },
): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    scryptCb(password, salt, keylen, opts, (err, derived) => {
      if (err) reject(err);
      else resolve(derived);
    });
  });
}

export async function hashPassword(password: string): Promise<string> {
  const salt = randomBytes(SALT_LEN);
  const derived = await scryptAsync(password, salt, KEY_LEN, { N, r, p });
  return `scrypt$N=${N}$r=${r}$p=${p}$${salt.toString('base64')}$${derived.toString('base64')}`;
}

export async function verifyPassword(password: string, stored: string): Promise<boolean> {
  const parts = stored.split('$');
  if (parts.length !== 6 || parts[0] !== 'scrypt') return false;
  const params = Object.fromEntries(
    parts.slice(1, 4).map((kv) => {
      const [k, v] = kv.split('=');
      return [k!, Number(v)];
    }),
  );
  const salt = Buffer.from(parts[4]!, 'base64');
  const expected = Buffer.from(parts[5]!, 'base64');
  const derived = await scryptAsync(password, salt, expected.length, {
    N: params['N']!,
    r: params['r']!,
    p: params['p']!,
  });
  return derived.length === expected.length && timingSafeEqual(derived, expected);
}
