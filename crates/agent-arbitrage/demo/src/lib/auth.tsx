'use client';

import { createContext, useCallback, useContext, useMemo, type ReactNode } from 'react';

/*
 * ── DEMO: auth hard-disabled ─────────────────────────────────────────────────
 * The demo is a static, backend-free showcase. Auth is forced OFF: there is no
 * login page, no token storage, and no network. Every consumer sees a fixed
 * DEV_SESSION so the operator UI renders immediately. The `login` / `logout`
 * methods are inert stubs kept only so the AuthContext shape is unchanged.
 */
export const AUTH_DISABLED = true;

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

export function AuthProvider({ children }: { children: ReactNode }) {
  // Always the stub session — no state, no effects, no network.
  const login = useCallback(async (): Promise<void> => {
    /* no-op: login is disabled in the demo */
  }, []);
  const logout = useCallback((): void => {
    /* no-op: nothing to sign out of in the demo */
  }, []);

  const value = useMemo<AuthContextValue>(
    () => ({ session: DEV_SESSION, login, logout, bootstrapping: false }),
    [login, logout],
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const v = useContext(AuthContext);
  if (!v) throw new Error('useAuth must be inside <AuthProvider>');
  return v;
}

export function getStoredToken(): string | null {
  return null; // no tokens in the demo
}
