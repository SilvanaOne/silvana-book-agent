'use client';

import { useEffect, useRef, useState, type FormEvent } from 'react';
import { useRouter } from 'next/navigation';
import { useAuth, AUTH_DISABLED } from '@/lib/auth';
import { Brand, NavTabs } from '@/components/chrome';
import { ThemeToggle } from '@/components/ThemeToggle';
import {
  fetchSpreads,
  fetchConfig,
  fetchStats,
  type SpreadDto,
  type RuntimeConfig,
  type Stats,
} from '@/lib/api';
import { advise, PRESET_QUESTIONS, type Advice, type AdviceContext } from '@/lib/assistant';

interface Turn {
  readonly id: number;
  readonly role: 'user' | 'ai';
  readonly text: string;
  readonly suggestion?: Advice['suggestion'];
}

export default function AssistantPage() {
  const router = useRouter();
  const { session, logout, bootstrapping } = useAuth();
  const [ctx, setCtx] = useState<AdviceContext>({ spreads: [], config: null, stats: null });
  const [turns, setTurns] = useState<readonly Turn[]>([
    {
      id: 0,
      role: 'ai',
      text:
        "I'm the advisory assistant. I read the same live feeds as the dashboard and suggest — I never place trades or change config. Ask me about current opportunities, or pick a prompt below.",
    },
  ]);
  const [input, setInput] = useState('');
  const nextId = useRef(1);
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!AUTH_DISABLED && !bootstrapping && !session) router.replace('/login');
  }, [bootstrapping, session, router]);

  useEffect(() => {
    if (!session) return;
    const ac = new AbortController();
    void (async () => {
      try {
        const [s, c, st] = await Promise.all([
          fetchSpreads(60, ac.signal),
          fetchConfig(ac.signal),
          fetchStats(ac.signal),
        ]);
        if (ac.signal.aborted) return;
        setCtx({ spreads: s.spreads as readonly SpreadDto[], config: c as RuntimeConfig, stats: st as Stats });
      } catch {
        /* advisory context is best-effort */
      }
    })();
    return () => ac.abort();
  }, [session]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [turns]);

  if (bootstrapping || !session) {
    return (
      <div className="app-bg">
        <div className="shell">
          <p className="empty">loading…</p>
        </div>
      </div>
    );
  }

  const ask = (question: string): void => {
    const text = question.trim();
    if (!text) return;
    const advice = advise(text, ctx);
    setTurns((prev) => [
      ...prev,
      { id: nextId.current++, role: 'user', text },
      { id: nextId.current++, role: 'ai', text: advice.text, suggestion: advice.suggestion },
    ]);
    setInput('');
  };

  const onSubmit = (e: FormEvent): void => {
    e.preventDefault();
    ask(input);
  };

  return (
    <div className="app-bg">
      <div className="shell reveal">
        <header className="topbar">
          <Brand />
          <NavTabs />
          <div className="statusline">
            <ThemeToggle />
            <span>{session.operator.username}</span>
            {AUTH_DISABLED ? null : (
              <button className="btn-ghost" onClick={logout}>
                sign out
              </button>
            )}
            <a
              className="hostbadge"
              href="https://silvana.one"
              target="_blank"
              rel="noreferrer"
              title="Hosted on Silvana"
            >
              <span>hosted on</span>
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img src="/silvana-logo.svg" alt="Silvana" />
            </a>
          </div>
        </header>

        <div className="card-flush" style={{ marginBottom: '1.25rem' }}>
          <h1 className="h-section" style={{ marginBottom: '0.4rem' }}>
            AI <span className="accent-text">Assistant</span>
          </h1>
          <p className="dim" style={{ margin: 0, fontSize: '0.9rem', lineHeight: 1.55 }}>
            Advisory layer only. The assistant explains and suggests; it never executes trades or
            mutates configuration — the hot trading path stays deterministic. This is a local
            heuristic preview; the Anthropic provider and RU locale arrive with the post-MVP AI
            module.
          </p>
          <div style={{ marginTop: '0.5rem' }}>
            <span className="badge">advisory · read-only</span>
          </div>
        </div>

        <div className="card-flush">
          <div className="chat">
            {turns.map((t) => (
              <div key={t.id} className={`msg ${t.role === 'user' ? 'msg-user' : 'msg-ai'}`}>
                {t.text}
                {t.suggestion ? (
                  <div className="suggestion">
                    <strong style={{ fontSize: '0.8rem' }}>{t.suggestion.label}</strong>
                    <div className="dim" style={{ fontSize: '0.83rem', marginTop: '0.2rem' }}>
                      {t.suggestion.detail}
                    </div>
                    <div className="mute" style={{ fontSize: '0.72rem', marginTop: '0.35rem' }}>
                      operator confirms — not applied automatically
                    </div>
                  </div>
                ) : null}
              </div>
            ))}
            <div ref={endRef} />
          </div>

          <div style={{ display: 'flex', flexWrap: 'wrap', gap: '0.4rem', margin: '0.9rem 0' }}>
            {PRESET_QUESTIONS.map((p) => (
              <button key={p} className="btn" style={{ fontSize: '0.78rem' }} onClick={() => ask(p)}>
                {p}
              </button>
            ))}
          </div>

          <form onSubmit={onSubmit} style={{ display: 'flex', gap: '0.5rem' }}>
            <input
              className="input"
              style={{ flex: 1, fontFamily: 'var(--font-body)' }}
              placeholder="Ask about spreads, risk, or venues…"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              aria-label="assistant input"
            />
            <button className="btn btn-accent" type="submit" disabled={!input.trim()}>
              ask
            </button>
          </form>
        </div>
      </div>
    </div>
  );
}
