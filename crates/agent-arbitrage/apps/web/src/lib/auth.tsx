'use client';

import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react';
import { rsaOaepEncrypt } from './crypto';

const API_BASE = process.env.NEXT_PUBLIC_API_URL ?? 'http://localhost:3000';
const STORAGE_KEY = 'arb.session';

/*
 * ── MVP toggle: login disabled ───────────────────────────────────────────────
 * The operator login is not needed for this MVP, so auth is bypassed by default.
 * All the auth machinery (login flow, RSA encrypt, token storage) stays intact —
 * it's just shunted. The matching server bypass is DISABLE_AUTH=1 (set by run.sh).
 *
 * To re-enable login: set NEXT_PUBLIC_DISABLE_AUTH=0 (web) + drop DISABLE_AUTH (api).
 */
export const AUTH_DISABLED = process.env.NEXT_PUBLIC_DISABLE_AUTH !== '0';

const DEV_SESSION: Session = {
  token: '',
  expiresAt: Number.MAX_SAFE_INTEGER,
  operator: { id: 'dev', username: 'operator', preferredLanguage: 'en' },
};

export interface Session {
  readonly token: string;
  readonly expiresAt: number;
  readonly operator: {
    readonly id: string;
    readonly username: string;
    readonly preferredLanguage: string;
  };
}

interface AuthContextValue {
  readonly session: Session | null;
  readonly login: (username: string, password: string) => Promise<void>;
  readonly logout: () => void;
  readonly bootstrapping: boolean;
}

const AuthContext = createContext<AuthContextValue | null>(null);

function loadSession(): Session | null {
  if (typeof window === 'undefined') return null;
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const s = JSON.parse(raw) as Session;
    if (typeof s.expiresAt === 'number' && s.expiresAt > Date.now()) return s;
    window.localStorage.removeItem(STORAGE_KEY);
    return null;
  } catch {
    return null;
  }
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [session, setSession] = useState<Session | null>(AUTH_DISABLED ? DEV_SESSION : null);
  const [bootstrapping, setBootstrapping] = useState(!AUTH_DISABLED);

  useEffect(() => {
    if (AUTH_DISABLED) return; // login bypassed for MVP — keep the stub session
    setSession(loadSession());
    setBootstrapping(false);
  }, []);

  const login = useCallback(async (username: string, password: string): Promise<void> => {
    const pubRes = await fetch(`${API_BASE}/api/auth/pubkey`);
    if (!pubRes.ok) throw new Error(`pubkey HTTP ${pubRes.status}`);
    const { publicKeyPem } = (await pubRes.json()) as { publicKeyPem: string };

    const encryptedPassword = await rsaOaepEncrypt(password, publicKeyPem);

    const res = await fetch(`${API_BASE}/api/auth/login`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username, encryptedPassword }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(body.error ?? `login HTTP ${res.status}`);
    }
    const data = (await res.json()) as Session;
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
    setSession(data);
  }, []);

  const logout = useCallback((): void => {
    if (AUTH_DISABLED) return; // nothing to sign out of while login is bypassed
    window.localStorage.removeItem(STORAGE_KEY);
    setSession(null);
  }, []);

  const value = useMemo<AuthContextValue>(
    () => ({ session, login, logout, bootstrapping }),
    [session, login, logout, bootstrapping],
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const v = useContext(AuthContext);
  if (!v) throw new Error('useAuth must be inside <AuthProvider>');
  return v;
}

export function getStoredToken(): string | null {
  if (AUTH_DISABLED) return null; // server bypasses auth too (DISABLE_AUTH=1)
  const s = loadSession();
  return s?.token ?? null;
}
