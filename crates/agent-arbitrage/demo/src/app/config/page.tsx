'use client';

import { useEffect, useState, type FormEvent } from 'react';
import { useRouter } from 'next/navigation';
import { useAuth, AUTH_DISABLED } from '@/lib/auth';
import { Brand, NavTabs } from '@/components/chrome';
import { ThemeToggle } from '@/components/ThemeToggle';
import { InfoTip } from '@/components/InfoTip';
import { VENUES } from '@/components/panels';
import { UI_TOKEN_PAIRS } from '@/lib/tokens';
import {
  fetchConfig,
  fetchVenueHealth,
  updateTradeConfig,
  updateRiskConfig,
  setKillSwitch,
  type RuntimeConfig,
  type VenueHealth,
} from '@/lib/api';

const POLL_MS = 5000;
const TOKEN_PAIRS = UI_TOKEN_PAIRS;

export default function ConfigPage() {
  const router = useRouter();
  const { session, bootstrapping } = useAuth();
  const [config, setConfig] = useState<RuntimeConfig | null>(null);
  const [venues, setVenues] = useState<readonly VenueHealth[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!AUTH_DISABLED && !bootstrapping && !session) router.replace('/login');
  }, [bootstrapping, session, router]);

  useEffect(() => {
    if (!session) return;
    const ac = new AbortController();
    const loadConfig = async (): Promise<void> => {
      try {
        const c = await fetchConfig(ac.signal);
        if (!ac.signal.aborted) {
          setConfig(c);
          setError(null);
        }
      } catch (e: unknown) {
        if (!ac.signal.aborted) setError(e instanceof Error ? e.message : String(e));
      }
    };
    const loadVenues = async (): Promise<void> => {
      try {
        const v = await fetchVenueHealth(ac.signal);
        if (!ac.signal.aborted) setVenues(v.venues);
      } catch {
        /* venues grid is optional — do not block trade/risk forms */
      }
    };
    const tick = (): void => {
      void loadConfig();
      void loadVenues();
    };
    tick();
    const id = setInterval(tick, POLL_MS);
    return () => {
      ac.abort();
      clearInterval(id);
    };
  }, [session]);

  if (bootstrapping || !session) {
    return (
      <div className="app-bg">
        <div className="shell">
          <p className="empty">loading…</p>
        </div>
      </div>
    );
  }

  const onStartStop = async (engage: boolean): Promise<void> => {
    if (!config || busy) return;
    setBusy(true);
    setError(null);
    try {
      await setKillSwitch(engage);
      setConfig(await fetchConfig());
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const running = config && !config.runtime.killSwitch;
  const venueByHealth = new Map(venues.map((v) => [v.venueId, v]));

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
          <span className="accent-text">Config</span> — parameters &amp; runtime
        </h1>
        <p className="dim" style={{ marginTop: 0, fontSize: '0.9rem' }}>
          Tune the scanner without restarting it. Mutations write to the DB and
          fire an audit log entry; the running scanner picks them up on its next cycle.
        </p>

        {/* Start / Stop hero */}
        <section
          className="card-flush section"
          style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '1rem', flexWrap: 'wrap' }}
        >
          <div>
            <div className="card-title" style={{ margin: 0 }}>
              Runtime
            </div>
            <div
              style={{
                fontFamily: 'var(--font-display)',
                fontSize: '1.4rem',
                fontWeight: 700,
                letterSpacing: '-0.02em',
                color: running ? 'var(--good)' : 'var(--bad)',
              }}
            >
              {running ? 'RUNNING' : 'STOPPED'}
            </div>
            <div className="mute" style={{ fontSize: '0.78rem' }}>
              {running
                ? 'scanner is detecting spreads — Stop pauses it'
                : 'kill switch engaged — Start resumes scanning'}
            </div>
          </div>
          <div style={{ display: 'flex', gap: '0.9rem', flexWrap: 'wrap' }}>
            <button
              type="button"
              className="btn-runtime"
              data-kind="start"
              onClick={() => void onStartStop(false)}
              disabled={!config || running === true || busy}
            >
              {running ? <span className="btn-runtime__pulse" /> : null}
              Start
            </button>
            <button
              type="button"
              className="btn-runtime"
              data-kind="stop"
              onClick={() => void onStartStop(true)}
              disabled={!config || running === false || busy}
            >
              Stop
            </button>
          </div>
        </section>

        {error ? (
          <p className="neg" style={{ marginTop: '0.6rem', fontSize: '0.85rem' }}>
            {error}
          </p>
        ) : null}

        {/* Editable trade + risk */}
        <section className="grid grid-auto section">
          <div className="card-flush">
            <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
              Trade parameters
              <InfoTip text="Minimum spread target + paper trade size. Mutations write to Config + AuditLog." />
            </h2>
            {config ? (
              <TradeForm
                initial={config.tradeConfig}
                onSaved={(c) => setConfig(c)}
                onError={(e) => setError(e)}
              />
            ) : (
              <p className="empty">loading…</p>
            )}
          </div>

          <div className="card-flush">
            <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
              Risk limits
              <InfoTip text="Pre-trade safety checks the deterministic risk module enforces." />
            </h2>
            {config ? (
              <RiskForm
                initial={config.riskConfig}
                onSaved={(c) => setConfig(c)}
                onError={(e) => setError(e)}
              />
            ) : (
              <p className="empty">loading…</p>
            )}
          </div>
        </section>

        {/* Venues */}
        <section className="card-flush section">
          <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
            Activated venues
            <InfoTip text="Each venue's runtime mode and live connection. Per-venue enable/disable is currently driven by env flags (ONESWAP_ENABLED=1 etc.); the live UI toggle ships with the executor wiring." />
          </h2>
          <div className="venue-grid">
            {VENUES.map((def) => {
              const h = venueByHealth.get(def.id);
              const active = !!h;
              return (
                <div className="venue-cell" key={def.id}>
                  <span
                    className="venue-logo"
                    style={
                      {
                        '--vc': def.color,
                        ...(def.glyph ? { '--glyph': `url(${def.glyph})` } : {}),
                      } as React.CSSProperties
                    }
                  >
                    {def.glyph ? <span className="venue-glyph" /> : <span className="venue-mono">{def.mono}</span>}
                  </span>
                  <span className="venue-meta">
                    <span className="venue-name">{def.name}</span>
                    <span className="venue-kind">{def.kind}</span>
                  </span>
                  <span className="venue-status" data-state={h?.status ?? 'down'}>
                    {active ? (h!.status === 'healthy' ? 'mock · live' : h!.status) : 'inactive'}
                  </span>
                </div>
              );
            })}
          </div>
        </section>

        {/* Tokens */}
        <section className="card-flush section">
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: '0.6rem',
              flexWrap: 'wrap',
              marginBottom: '0.9rem',
            }}
          >
            <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem', margin: 0 }}>
              Tokens to arbitrage
              <InfoTip text="Quoted pairs the scanner watches across the activated venues. Tokens are derived from the venue catalogue; user-editable token control lands with executor wiring." />
            </h2>
            {/* Decorative for now — real catalogue editing arrives with the
                executor wiring. No onClick handler intentionally. */}
            <button
              type="button"
              className="btn btn-add"
              aria-label="Add coin (preview only)"
              title="Preview — token catalogue editing lands with the executor wiring"
            >
              <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden>
                <path d="M8 3v10M3 8h10" />
              </svg>
              Add coin
            </button>
          </div>
          <div className="table-wrap">
            <table className="data">
              <thead>
                <tr>
                  <th>base</th>
                  <th>quote</th>
                  <th>cluster</th>
                  <th>venues</th>
                </tr>
              </thead>
              <tbody>
                {TOKEN_PAIRS.map((p) => {
                  // venues that supply this exact base/quote (per supportsPair mocks)
                  const supplying = VENUES.filter((v) => {
                    if (v.id === 'bybit' || v.id === 'kucoin') return p.base === 'CC' && p.quote === 'USDT';
                    if (v.id === 'oneswap')
                      return p.base === 'CC' && (p.quote === 'USDCx' || p.quote === 'CBTC' || p.quote === 'CETH');
                    if (v.id === 'temple')
                      return (
                        (p.quote === 'USDCx' && (p.base === 'CC' || p.base === 'CBTC')) ||
                        (p.quote === 'USDC' && (p.base === 'CC' || p.base === 'CBTC' || p.base === 'CETH'))
                      );
                    if (v.id === 'cantex') return p.base === 'CC' && (p.quote === 'USDCx' || p.quote === 'USDC' || p.quote === 'CETH');
                    if (v.id === 'silvana')
                      return (p.base === 'CC' || p.base === 'CBTC' || p.base === 'CETH') && p.quote === 'USDC';
                    return false;
                  });
                  return (
                    <tr key={`${p.base}-${p.quote}`}>
                      <td>{p.base}</td>
                      <td>{p.quote}</td>
                      <td>{p.cluster}</td>
                      <td>
                        <span style={{ display: 'inline-flex', flexWrap: 'wrap', gap: '0.3rem' }}>
                          {supplying.map((v) => (
                            <span
                              key={v.id}
                              className="badge"
                              style={{
                                borderColor: 'transparent',
                                background: `color-mix(in srgb, ${v.color} 14%, transparent)`,
                                color: v.color,
                              }}
                            >
                              {v.name}
                            </span>
                          ))}
                        </span>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </section>

        {/* Reference: scanner-level (env) parameters */}
        <section className="card-flush section">
          <h2 className="h-section">Scanner runtime (env)</h2>
          <div className="grid grid-auto">
            <Stat label="strategy mode" value={config?.runtime.strategyMode ?? '—'} />
            <Stat label="silvana host" value={config?.runtime.silvanaHostMode ?? '—'} />
            <Stat label="scan interval" value={config ? `${config.runtime.scanIntervalMs} ms` : '—'} />
            <Stat label="persist" value={config?.runtime.scannerPersist ? 'on' : 'off'} />
          </div>
        </section>
      </div>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="card">
      <div className="card-title">{label}</div>
      <div className="mono" style={{ fontFamily: 'var(--font-display)', fontSize: '1.1rem' }}>
        {value}
      </div>
    </div>
  );
}

function NumField({ label, value, onChange }: { label: string; value: string; onChange: (v: string) => void }) {
  return (
    <label className="field">
      <span>{label}</span>
      <input className="input" type="number" step="any" value={value} onChange={(e) => onChange(e.target.value)} />
    </label>
  );
}

function TradeForm({
  initial,
  onSaved,
  onError,
}: {
  initial: RuntimeConfig['tradeConfig'];
  onSaved: (c: RuntimeConfig) => void;
  onError: (e: string) => void;
}) {
  const [t, setT] = useState(String(initial.targetSpreadPercent));
  const [s, setS] = useState(String(initial.tradeSizeUsd));
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setBusy(true);
    try {
      await updateTradeConfig({
        targetSpreadPercent: Number(t),
        tradeSizeUsd: Number(s),
      });
      onSaved(await fetchConfig());
    } catch (err: unknown) {
      onError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form onSubmit={(e) => void submit(e)} style={{ display: 'flex', flexDirection: 'column', gap: '0.5rem' }}>
      <NumField label="min. spread (%) — target" value={t} onChange={setT} />
      <NumField label="arbitrage amount ($)" value={s} onChange={setS} />
      <button className="btn btn-accent" type="submit" disabled={busy} style={{ marginTop: '0.4rem' }}>
        {busy ? 'saving…' : 'save trade config'}
      </button>
    </form>
  );
}

function RiskForm({
  initial,
  onSaved,
  onError,
}: {
  initial: RuntimeConfig['riskConfig'];
  onSaved: (c: RuntimeConfig) => void;
  onError: (e: string) => void;
}) {
  const [age, setAge] = useState(String(initial.maxQuoteAgeMs));
  const [dl, setDl] = useState(String(initial.dailyLossLimitUsd));
  const [cl, setCl] = useState(String(initial.maxConsecutiveLosses));
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setBusy(true);
    try {
      await updateRiskConfig({
        maxQuoteAgeMs: Number(age),
        dailyLossLimitUsd: Number(dl),
        maxConsecutiveLosses: Number(cl),
      });
      onSaved(await fetchConfig());
    } catch (err: unknown) {
      onError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form onSubmit={(e) => void submit(e)} style={{ display: 'flex', flexDirection: 'column', gap: '0.5rem' }}>
      <NumField label="max quote age (ms)" value={age} onChange={setAge} />
      <NumField label="daily loss limit ($)" value={dl} onChange={setDl} />
      <NumField label="max consec. losses" value={cl} onChange={setCl} />
      <button className="btn btn-accent" type="submit" disabled={busy} style={{ marginTop: '0.4rem' }}>
        {busy ? 'saving…' : 'save risk limits'}
      </button>
    </form>
  );
}
