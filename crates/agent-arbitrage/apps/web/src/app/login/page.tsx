'use client';

import { useState, type FormEvent } from 'react';
import { useRouter } from 'next/navigation';
import { useAuth, AUTH_DISABLED } from '@/lib/auth';
import { Brand } from '@/components/chrome';

export default function LoginPage() {
  const router = useRouter();
  const { login, session } = useAuth();
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Login is disabled for this MVP (NEXT_PUBLIC_DISABLE_AUTH) — or already
  // signed in: either way, bounce to the dashboard. The form never renders.
  if (AUTH_DISABLED || session) {
    if (typeof window !== 'undefined') router.replace('/');
    return null;
  }

  const onSubmit = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await login(username, password);
      router.replace('/');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="app-bg login-wrap">
      <form className="card-flush login-card reveal" onSubmit={(e) => void onSubmit(e)}>
        <div style={{ marginBottom: '0.4rem' }}>
          <Brand />
        </div>
        <h1 className="h-section" style={{ margin: 0 }}>
          Operator <span className="accent-text">login</span>
        </h1>
        <input
          className="input"
          style={{ width: '100%', fontFamily: 'var(--font-body)' }}
          type="text"
          placeholder="username"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          autoComplete="username"
          required
        />
        <input
          className="input"
          style={{ width: '100%', fontFamily: 'var(--font-body)' }}
          type="password"
          placeholder="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete="current-password"
          required
        />
        {error ? (
          <div className="neg" style={{ fontSize: '0.82rem' }}>
            {error}
          </div>
        ) : null}
        <button className="btn btn-accent" type="submit" disabled={busy}>
          {busy ? 'signing in…' : 'sign in'}
        </button>
        <p className="mute" style={{ fontSize: '0.72rem', margin: 0 }}>
          Password is RSA-OAEP-256 encrypted before it leaves the browser.
        </p>
      </form>
    </div>
  );
}
