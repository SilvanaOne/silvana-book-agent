'use client';

import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { useAuth, AUTH_DISABLED } from '@/lib/auth';
import { Brand, NavTabs } from '@/components/chrome';
import { ThemeToggle } from '@/components/ThemeToggle';
import { InfoTip } from '@/components/InfoTip';

/**
 * AI Manager — delegation layout (preview).
 *
 * Where the AI Assistant only suggests, the AI Manager would *act* within
 * operator-pre-authorised bounds: auto-tune target spread inside a band, pause
 * scanning on anomalies, rotate tokens, etc. Today nothing actually executes —
 * delegations persist to localStorage only and are surfaced read-only. The
 * Anthropic provider + the actual policy engine arrive with the post-MVP AI
 * module (md_docs/13-ai-assistant-expansion.md). Hot trading path stays
 * deterministic; operator can revoke any delegation at any time.
 */

interface Capability {
  readonly id: string;
  readonly title: string;
  readonly description: string;
  readonly scope: string;
}

const CAPABILITIES: readonly Capability[] = [
  {
    id: 'auto-target-spread',
    title: 'Auto-tune target spread',
    description:
      'Adjust min-spread target inside the configured band based on recent hit-rate, never above max.',
    scope: 'Trade config',
  },
  {
    id: 'auto-pause-on-anomaly',
    title: 'Auto-pause on anomaly',
    description:
      'Engage the kill switch when venues flap, spreads diverge from norm, or risk rejections spike.',
    scope: 'Kill switch',
  },
  {
    id: 'auto-rotate-tokens',
    title: 'Auto-rotate watch list',
    description:
      'Drop quiet pairs and promote active ones from the venue catalogue when activity shifts.',
    scope: 'Token set',
  },
  {
    id: 'auto-venue-failover',
    title: 'Venue failover',
    description:
      'Mark a venue inactive after repeated errors / staleness, re-activate when health returns.',
    scope: 'Venue activation',
  },
  {
    id: 'auto-confirm-low-risk',
    title: 'Auto-confirm low-risk trades',
    description:
      'When the executor is wired, auto-confirm trades inside a tight spread+size band; everything else still needs operator OK.',
    scope: 'Executor (post-MVP)',
  },
  {
    id: 'auto-rebalance',
    title: 'Inventory auto-rebalance',
    description:
      'Trigger the rebalancer when per-venue inventory drifts past thresholds. Routes via stable-router v2/3.',
    scope: 'Rebalancer (post-MVP)',
  },
];

const KEY = 'arb.manager.delegations';

function loadDelegations(): Record<string, boolean> {
  if (typeof window === 'undefined') return {};
  try {
    return JSON.parse(window.localStorage.getItem(KEY) ?? '{}') as Record<string, boolean>;
  } catch {
    return {};
  }
}

interface Decision {
  readonly ts: string;
  readonly cap: string;
  readonly state: 'granted' | 'revoked';
}

export default function ManagerPage() {
  const router = useRouter();
  const { session, bootstrapping } = useAuth();
  const [delegations, setDelegations] = useState<Record<string, boolean>>({});
  const [decisions, setDecisions] = useState<readonly Decision[]>([]);

  useEffect(() => {
    if (!AUTH_DISABLED && !bootstrapping && !session) router.replace('/login');
  }, [bootstrapping, session, router]);

  useEffect(() => {
    setDelegations(loadDelegations());
  }, []);

  if (bootstrapping || !session) {
    return (
      <div className="app-bg">
        <div className="shell">
          <p className="empty">loading…</p>
        </div>
      </div>
    );
  }

  const granted = CAPABILITIES.filter((c) => delegations[c.id]).length;

  const toggle = (id: string): void => {
    setDelegations((prev) => {
      const next = { ...prev, [id]: !prev[id] };
      try {
        window.localStorage.setItem(KEY, JSON.stringify(next));
      } catch {
        /* ignore quota / private mode */
      }
      setDecisions((d) =>
        [
          {
            ts: new Date().toISOString(),
            cap: id,
            state: next[id] ? 'granted' : 'revoked',
          } as Decision,
          ...d,
        ].slice(0, 12),
      );
      return next;
    });
  };

  const revokeAll = (): void => {
    setDelegations({});
    try {
      window.localStorage.setItem(KEY, '{}');
    } catch {
      /* ignore */
    }
    setDecisions((d) =>
      [{ ts: new Date().toISOString(), cap: 'ALL', state: 'revoked' } as Decision, ...d].slice(0, 12),
    );
  };

  return (
    <div className="app-bg">
      <div className="shell reveal">
        <header className="topbar">
          <Brand />
          <NavTabs />
          <div style={{ display: 'flex', alignItems: 'center', gap: '0.9rem' }}>
            <ThemeToggle />
            <a className="hostbadge" href="https://silvana.one" target="_blank" rel="noreferrer" title="Hosted on Silvana">
              <span>hosted on</span>
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img src="/silvana-logo.svg" alt="Silvana" />
            </a>
          </div>
        </header>

        <h1 className="h-page" style={{ marginBottom: '0.3rem' }}>
          <span className="accent-text">AI Manager</span> — delegated supervision
        </h1>
        <p className="dim" style={{ marginTop: 0, fontSize: '0.9rem', maxWidth: 760 }}>
          Pre-authorise the AI to act on your behalf inside bounded capabilities. The Assistant
          tab only suggests; the Manager would <em>do</em>. Today this is a UI preview —
          delegations are saved locally and inert. The policy engine wires in with the post-MVP
          AI module (md_docs/13-ai-assistant-expansion.md). Hot trading path stays deterministic;
          revoke at any time.
        </p>

        <section
          className="card-flush section"
          style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '1rem', flexWrap: 'wrap' }}
        >
          <div>
            <div className="card-title" style={{ margin: 0 }}>
              Status
            </div>
            <div
              style={{
                fontFamily: 'var(--font-display)',
                fontSize: '1.25rem',
                fontWeight: 700,
                color: granted > 0 ? 'var(--accent-2)' : 'var(--text-dim)',
              }}
            >
              {granted > 0
                ? `Supervising ${granted}/${CAPABILITIES.length} task${granted === 1 ? '' : 's'} · preview`
                : 'Idle — no delegations granted'}
            </div>
            <div className="mute" style={{ fontSize: '0.78rem' }}>
              advisory · read-only · the operator stays in the loop
            </div>
          </div>
          <div style={{ display: 'flex', gap: '0.6rem', flexWrap: 'wrap' }}>
            <span className="badge">preview · not wired</span>
            {granted > 0 ? (
              <button className="btn btn-danger" onClick={revokeAll}>
                revoke all
              </button>
            ) : null}
          </div>
        </section>

        {/* Simulation controls — UI only, no functionality. Reserved for the
            post-MVP AI module: run the Manager against synthetic/test wrappers
            ("фантики") to inspect how it drives the agent before granting any
            production authority. */}
        <section
          className="card-flush section"
          style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '1rem', flexWrap: 'wrap' }}
        >
          <div>
            <div className="card-title" style={{ margin: 0, display: 'inline-flex', alignItems: 'center', gap: '0.35rem' }}>
              Simulation
              <InfoTip text="Future: run the AI Manager against synthetic / paper wrappers to verify how it would steer the agent — no real funds, no real venues — before granting it any production authority. Today these controls are UI only." />
            </div>
            <div style={{ fontFamily: 'var(--font-display)', fontSize: '1.05rem', color: 'var(--text-dim)' }}>
              dry-run sandbox — preview
            </div>
            <div className="mute" style={{ fontSize: '0.78rem' }}>
              run the Manager against test wrappers ("фантики") to inspect its decisions safely
            </div>
          </div>
          <div style={{ display: 'flex', gap: '0.65rem', flexWrap: 'wrap' }}>
            <button
              type="button"
              className="btn-sim"
              data-kind="simulate"
              aria-label="Simulate (preview only)"
              title="Preview — wires up with the post-MVP AI module"
            >
              <svg viewBox="0 0 16 16" width="13" height="13" fill="currentColor" aria-hidden>
                <path d="M4 2.5v11l10-5.5L4 2.5z" />
              </svg>
              Simulate
            </button>
            <button
              type="button"
              className="btn-sim"
              data-kind="stop"
              aria-label="Stop simulation (preview only)"
              title="Preview — wires up with the post-MVP AI module"
            >
              <svg viewBox="0 0 16 16" width="13" height="13" fill="currentColor" aria-hidden>
                <rect x="3.5" y="3.5" width="9" height="9" rx="1.5" />
              </svg>
              Stop Simulation
            </button>
          </div>
        </section>

        <section className="grid grid-auto section">
          {CAPABILITIES.map((c) => {
            const on = !!delegations[c.id];
            return (
              <div className="card" key={c.id}>
                <div className="card-title" style={{ alignItems: 'flex-start' }}>
                  <span style={{ display: 'inline-flex', alignItems: 'center', gap: '0.35rem' }}>
                    {c.title}
                    <InfoTip text={c.description} />
                  </span>
                  <DelegationSwitch on={on} onToggle={() => toggle(c.id)} label={c.title} />
                </div>
                <p className="dim" style={{ margin: '0.1rem 0 0.5rem', fontSize: '0.84rem', lineHeight: 1.5 }}>
                  {c.description}
                </p>
                <div className="kv" style={{ fontSize: '0.78rem' }}>
                  <span>scope</span>
                  <span>{c.scope}</span>
                </div>
                <div className="kv" style={{ fontSize: '0.78rem' }}>
                  <span>delegation</span>
                  <span style={{ color: on ? 'var(--good)' : 'var(--text-mute)' }}>
                    {on ? 'GRANTED' : 'not granted'}
                  </span>
                </div>
              </div>
            );
          })}
        </section>

        <section className="card-flush section">
          <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
            Delegation log
            <InfoTip text="Local-only log of grant/revoke actions in this browser session. The real audit log lives in the dashboard." />
          </h2>
          {decisions.length === 0 ? (
            <p className="empty">no delegation actions yet — toggle a capability above</p>
          ) : (
            <div className="table-wrap">
              <table className="data">
                <thead>
                  <tr>
                    <th>time</th>
                    <th>capability</th>
                    <th>action</th>
                  </tr>
                </thead>
                <tbody>
                  {decisions.map((d, i) => (
                    <tr key={`${d.ts}-${i}`}>
                      <td>{new Date(d.ts).toLocaleTimeString()}</td>
                      <td>{d.cap}</td>
                      <td style={{ color: d.state === 'granted' ? 'var(--good)' : 'var(--bad)' }}>
                        {d.state}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}

function DelegationSwitch({
  on,
  onToggle,
  label,
}: {
  on: boolean;
  onToggle: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={`${on ? 'revoke' : 'grant'} ${label}`}
      onClick={onToggle}
      className="theme-toggle"
      style={{ marginLeft: '0.5rem' }}
    >
      <span className="theme-toggle__track" style={{ background: on ? 'var(--accent-soft)' : undefined, borderColor: on ? 'var(--accent-line)' : undefined }}>
        <span
          className="theme-toggle__thumb"
          data-side={on ? 'right' : 'left'}
          style={{ background: on ? 'var(--accent-grad)' : 'var(--surface-3)', color: on ? 'var(--text-on-accent)' : 'var(--text-mute)' }}
        >
          {on ? '✓' : '·'}
        </span>
      </span>
    </button>
  );
}
