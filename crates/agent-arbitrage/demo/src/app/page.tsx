'use client';

import { useEffect, useState, type FormEvent } from 'react';
import { useRouter } from 'next/navigation';
import {
  fetchSpreads,
  fetchConfig,
  subscribeToSpreads,
  updateTradeConfig,
  updateRiskConfig,
  setKillSwitch,
  type SpreadDto,
  type RuntimeConfig,
} from '@/lib/api';
import { useAuth, AUTH_DISABLED } from '@/lib/auth';
import { demoSupplementalSpreads } from '@/lib/demo';
import { Brand, NavTabs } from '@/components/chrome';
import { ThemeToggle } from '@/components/ThemeToggle';
import { Card, Kv, trim } from '@/components/panels';
import { InfoTip } from '@/components/InfoTip';
import { SpreadChart } from '@/components/SpreadChart';
import {
  VenueWidget,
  StatsPanel,
  PnlPanel,
  TradeLogPanel,
  InventoryPanel,
  AuditLogPanel,
} from '@/components/panels';

const CONFIG_REFRESH_MS = 5000;
const MAX_ROWS = 120;

export default function HomePage() {
  const router = useRouter();
  const { session, logout, bootstrapping } = useAuth();
  const [spreads, setSpreads] = useState<readonly SpreadDto[]>([]);
  const [config, setConfig] = useState<RuntimeConfig | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);
  const [streamLive, setStreamLive] = useState(false);

  useEffect(() => {
    if (!AUTH_DISABLED && !bootstrapping && !session) router.replace('/login');
  }, [bootstrapping, session, router]);

  useEffect(() => {
    if (!session) return;
    const ac = new AbortController();
    void (async () => {
      try {
        const s = await fetchSpreads(60, ac.signal);
        if (ac.signal.aborted) return;
        setSpreads(s.spreads);
        setLastUpdated(new Date());
        setError(null);
      } catch (err: unknown) {
        if (ac.signal.aborted) return;
        setError(err instanceof Error ? err.message : String(err));
      }
    })();

    const unsubscribe = subscribeToSpreads({
      onOpen: () => {
        setStreamLive(true);
        setError(null);
      },
      onError: () => setStreamLive(false),
      onSpread: (s) => {
        setSpreads((prev) => {
          if (prev.length > 0 && prev[0]!.id === s.id) return prev;
          return [s, ...prev].slice(0, MAX_ROWS);
        });
        setLastUpdated(new Date());
      },
    });
    return () => {
      ac.abort();
      unsubscribe();
    };
  }, [session]);

  useEffect(() => {
    if (!session) return;
    const ac = new AbortController();
    const tick = async (): Promise<void> => {
      try {
        const c = await fetchConfig(ac.signal);
        if (!ac.signal.aborted) setConfig(c);
      } catch (err: unknown) {
        if (!ac.signal.aborted) setError(err instanceof Error ? err.message : String(err));
      }
    };
    void tick();
    const id = setInterval(() => void tick(), CONFIG_REFRESH_MS);
    return () => {
      ac.abort();
      clearInterval(id);
    };
  }, [session]);

  // Blend in supplemental demo rows so the table always carries CBTC/CETH/USDCx
  // examples until the scanner has cross-venue pairs in mock. Sorted by ts
  // desc; demo rows are tagged via id prefix.
  const mergedSpreads = [...spreads, ...demoSupplementalSpreads()].sort(
    (a, b) => new Date(b.ts).getTime() - new Date(a.ts).getTime(),
  );

  if (bootstrapping || !session) {
    return (
      <div className="app-bg">
        <div className="shell">
          <p className="empty">loading…</p>
        </div>
      </div>
    );
  }

  const onKillSwitchToggle = async (): Promise<void> => {
    if (!config) return;
    try {
      await setKillSwitch(!config.runtime.killSwitch);
      setConfig(await fetchConfig());
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <div className="app-bg">
      <div className="shell reveal">
        <header className="topbar">
          <Brand />
          <NavTabs />
          <div style={{ display: 'flex', alignItems: 'center', gap: '1.25rem', flexWrap: 'wrap' }}>
            <div className="statusline">
              <span style={{ display: 'inline-flex', alignItems: 'center', gap: '0.4rem' }}>
                <span className={`dot ${streamLive ? 'dot-good' : 'dot-idle'}`} />
                {streamLive ? 'live' : 'offline'}
              </span>
              <span className="mute">{lastUpdated ? lastUpdated.toLocaleTimeString() : '—'}</span>
              {error ? <span className="neg">err: {error}</span> : null}
              <span className="mute">·</span>
              <span>{session.operator.username}</span>
              {AUTH_DISABLED ? null : (
                <button className="btn-ghost" onClick={logout}>
                  sign out
                </button>
              )}
            </div>
            <ThemeToggle />
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

        {config ? (
          <div className="killswitch" data-engaged={config.runtime.killSwitch}>
            <div>
              <div className="card-title" style={{ margin: 0, display: 'inline-flex', alignItems: 'center', gap: '0.35rem' }}>
                Kill switch
                <InfoTip text="Master pause. When engaged, the scanner stops detecting and any wired executor halts — opportunities are ignored until released." />
              </div>
              <div
                style={{
                  fontFamily: 'var(--font-display)',
                  fontSize: '1.05rem',
                  color: config.runtime.killSwitch ? 'var(--bad)' : 'var(--good)',
                }}
              >
                {config.runtime.killSwitch ? 'ENGAGED · scanner paused' : 'released · scanner running'}
              </div>
            </div>
            <button
              className={`btn ${config.runtime.killSwitch ? 'btn-accent' : 'btn-danger'}`}
              onClick={() => void onKillSwitchToggle()}
              style={{ textTransform: 'uppercase', letterSpacing: '0.04em' }}
            >
              {config.runtime.killSwitch ? 'release' : 'engage'}
            </button>
          </div>
        ) : null}

        {config ? (
          <ConfigPanel config={config} onSaved={setConfig} onError={setError} />
        ) : null}

        <VenueWidget />

        <section className="section grid grid-auto">
          <StatsPanel />
          <PnlPanel />
        </section>

        <SpreadChart spreads={spreads} />

        <section className="section">
          <h2 className="h-section">Recent spreads</h2>
          <SpreadsTable spreads={mergedSpreads} />
        </section>

        <TradeLogPanel />
        <InventoryPanel />
        <AuditLogPanel />
      </div>
    </div>
  );
}

function ConfigPanel({
  config,
  onSaved,
  onError,
}: {
  config: RuntimeConfig;
  onSaved: (c: RuntimeConfig) => void;
  onError: (e: string) => void;
}) {
  const [editing, setEditing] = useState<'trade' | 'risk' | null>(null);

  return (
    <section className="grid grid-3">
      <Card title="Runtime">
        <Kv k="strategy mode" v={config.runtime.strategyMode} />
        <Kv k="silvana host" v={config.runtime.silvanaHostMode} />
        <Kv k="scan interval" v={`${config.runtime.scanIntervalMs} ms`} />
        <Kv k="persist" v={config.runtime.scannerPersist ? 'on' : 'off'} />
      </Card>

      <Card
        title="Trade config"
        action={
          <button className="btn-ghost" onClick={() => setEditing(editing === 'trade' ? null : 'trade')}>
            {editing === 'trade' ? 'cancel' : 'edit'}
          </button>
        }
      >
        {editing === 'trade' ? (
          <TradeForm
            initial={config.tradeConfig}
            onSubmit={async (patch) => {
              try {
                await updateTradeConfig(patch);
                onSaved(await fetchConfig());
                setEditing(null);
              } catch (err: unknown) {
                onError(err instanceof Error ? err.message : String(err));
              }
            }}
          />
        ) : (
          <>
            <Kv k="min. spread" v={`${config.tradeConfig.targetSpreadPercent}%`} />
            <Kv k="trade size" v={`$${config.tradeConfig.tradeSizeUsd}`} />
          </>
        )}
      </Card>

      <Card
        title="Risk limits"
        action={
          <button className="btn-ghost" onClick={() => setEditing(editing === 'risk' ? null : 'risk')}>
            {editing === 'risk' ? 'cancel' : 'edit'}
          </button>
        }
      >
        {editing === 'risk' ? (
          <RiskForm
            initial={config.riskConfig}
            onSubmit={async (patch) => {
              try {
                await updateRiskConfig(patch);
                onSaved(await fetchConfig());
                setEditing(null);
              } catch (err: unknown) {
                onError(err instanceof Error ? err.message : String(err));
              }
            }}
          />
        ) : (
          <>
            <Kv k="max quote age" v={`${config.riskConfig.maxQuoteAgeMs} ms`} />
            <Kv k="daily loss limit" v={`$${config.riskConfig.dailyLossLimitUsd}`} />
            <Kv k="max consec losses" v={config.riskConfig.maxConsecutiveLosses} />
          </>
        )}
      </Card>
    </section>
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
  onSubmit,
}: {
  initial: RuntimeConfig['tradeConfig'];
  onSubmit: (patch: Partial<RuntimeConfig['tradeConfig']>) => Promise<void>;
}) {
  const [t, setT] = useState(String(initial.targetSpreadPercent));
  const [s, setS] = useState(String(initial.tradeSizeUsd));
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setBusy(true);
    await onSubmit({ targetSpreadPercent: Number(t), tradeSizeUsd: Number(s) });
    setBusy(false);
  };
  return (
    <form onSubmit={(e) => void submit(e)} style={{ display: 'flex', flexDirection: 'column', gap: '0.45rem' }}>
      <NumField label="min. spread %" value={t} onChange={setT} />
      <NumField label="trade size USD" value={s} onChange={setS} />
      <button className="btn btn-accent" type="submit" disabled={busy} style={{ marginTop: '0.3rem' }}>
        {busy ? 'saving…' : 'save'}
      </button>
    </form>
  );
}

function RiskForm({
  initial,
  onSubmit,
}: {
  initial: RuntimeConfig['riskConfig'];
  onSubmit: (patch: Partial<RuntimeConfig['riskConfig']>) => Promise<void>;
}) {
  const [age, setAge] = useState(String(initial.maxQuoteAgeMs));
  const [dl, setDl] = useState(String(initial.dailyLossLimitUsd));
  const [cl, setCl] = useState(String(initial.maxConsecutiveLosses));
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setBusy(true);
    await onSubmit({
      maxQuoteAgeMs: Number(age),
      dailyLossLimitUsd: Number(dl),
      maxConsecutiveLosses: Number(cl),
    });
    setBusy(false);
  };
  return (
    <form onSubmit={(e) => void submit(e)} style={{ display: 'flex', flexDirection: 'column', gap: '0.45rem' }}>
      <NumField label="max quote age ms" value={age} onChange={setAge} />
      <NumField label="daily loss USD" value={dl} onChange={setDl} />
      <NumField label="max consec losses" value={cl} onChange={setCl} />
      <button className="btn btn-accent" type="submit" disabled={busy} style={{ marginTop: '0.3rem' }}>
        {busy ? 'saving…' : 'save'}
      </button>
    </form>
  );
}

export function SpreadsTable({ spreads }: { spreads: readonly SpreadDto[] }) {
  if (spreads.length === 0) {
    return (
      <p className="empty">
        no spreads yet — start the scanner with <code>SCANNER_PERSIST=1</code>
      </p>
    );
  }
  return (
    <div className="table-wrap">
      <table className="data">
        <thead>
          <tr>
            <th>time</th>
            <th>pair</th>
            <th>route</th>
            <th className="num">spread</th>
            <th className="num">buy@</th>
            <th className="num">sell@</th>
            <th className="num">est. profit</th>
            <th>acted</th>
          </tr>
        </thead>
        <tbody>
          {spreads.map((s) => {
            const crossCluster = s.basePairKey.includes('~');
            const isDemo = s.id.startsWith('demo-');
            return (
              <tr key={s.id}>
                <td>{new Date(s.ts).toLocaleTimeString()}</td>
                <td>
                  {s.basePairKey}
                  {crossCluster ? (
                    <span className="badge" style={{ marginLeft: '0.4rem' }}>
                      cross-cluster
                    </span>
                  ) : null}
                  {isDemo ? (
                    <span className="badge" data-demo style={{ marginLeft: '0.4rem' }}>
                      demo
                    </span>
                  ) : null}
                </td>
                <td>
                  {s.buyVenueId} → {s.sellVenueId}
                </td>
                <td className="num accent-text" style={{ fontWeight: 700 }}>
                  {s.spreadBps} bps
                </td>
                <td className="num">{trim(s.buyPrice, 6)}</td>
                <td className="num">{trim(s.sellPrice, 6)}</td>
                <td className="num pos">${trim(s.estProfitUsd, 4)}</td>
                <td>{s.acted ? '✓' : '·'}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
