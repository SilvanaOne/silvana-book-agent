'use client';

import { useEffect, useState, type ReactNode, type CSSProperties } from 'react';
import { InfoTip } from './InfoTip';
import { asset } from '@/lib/asset';
import {
  fetchVenueHealth,
  fetchStats,
  fetchAuditLog,
  fetchTrades,
  fetchInventory,
  type VenueHealth,
  type Stats,
  type AuditEntry,
  type TradeEntry,
  type Holding,
} from '@/lib/api';

const POLL_MS = 5000;

/** Poll a fetcher on an interval; returns latest value (or null until first load). */
function usePoll<T>(fetcher: (signal: AbortSignal) => Promise<T>, deps: unknown[] = []): T | null {
  const [data, setData] = useState<T | null>(null);
  useEffect(() => {
    const ac = new AbortController();
    const tick = async (): Promise<void> => {
      try {
        const v = await fetcher(ac.signal);
        if (!ac.signal.aborted) setData(v);
      } catch {
        /* transient — keep last good value */
      }
    };
    void tick();
    const id = setInterval(() => void tick(), POLL_MS);
    return () => {
      ac.abort();
      clearInterval(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return data;
}

/**
 * Static catalogue of the venues the agent trades. Brand colour + glyph drive
 * the logo tile; live connection state comes from /api/venues/health (a venue
 * absent from the feed = not yet seen = offline). kucoin + silvana render real
 * monochrome brand glyphs (tinted via CSS mask); the Canton DEXes that have no
 * public icon-CDN entry use a brand-coloured monogram in the same tile.
 */
interface VenueDef {
  readonly id: string;
  readonly name: string;
  readonly kind: string;
  readonly color: string;
  readonly glyph?: string; // /venues/<id>.svg (monochrome) — else `mono`
  readonly mono?: string;
}

export const VENUES: readonly VenueDef[] = [
  // Silvana first — it's the host runtime / flagship venue across all UI lists.
  { id: 'silvana', name: 'Silvana Book', kind: 'Canton · CLOB', color: '#D6448F', glyph: '/venues/silvana.svg' },
  { id: 'temple', name: 'Temple', kind: 'Canton · CLOB', color: '#C9A227', mono: 'Te' },
  { id: 'cantex', name: 'Cantex', kind: 'Canton · AMM', color: '#7C6CF0', mono: 'Cx' },
  { id: 'oneswap', name: 'OneSwap', kind: 'Canton · AMM', color: '#2EC4B6', mono: '1S' },
  { id: 'bybit', name: 'Bybit', kind: 'CEX · spot', color: '#F7A600', mono: 'By' },
  { id: 'kucoin', name: 'KuCoin', kind: 'CEX · spot', color: '#23AF91', glyph: '/venues/kucoin.svg' },
];

/** Sort key that always lifts silvana to the top regardless of any other ordering. */
export function venueSortKey(id: string): number {
  if (id === 'silvana') return 0;
  const ix = VENUES.findIndex((v) => v.id === id);
  return ix >= 0 ? ix + 1 : 99;
}

const STATUS_LABEL: Record<VenueHealth['status'], string> = {
  healthy: 'connected',
  stale: 'stale',
  down: 'offline',
};
const STATUS_DOT: Record<VenueHealth['status'], string> = {
  healthy: 'dot-good',
  stale: 'dot-warn',
  down: 'dot-bad',
};

function VenueLogo({ def }: { def: VenueDef }) {
  const style = {
    '--vc': def.color,
    ...(def.glyph ? { '--glyph': `url(${asset(def.glyph)})` } : {}),
  } as CSSProperties;
  return (
    <span className="venue-logo" style={style}>
      {def.glyph ? <span className="venue-glyph" /> : <span className="venue-mono">{def.mono}</span>}
    </span>
  );
}

export function VenueWidget() {
  const data = usePoll((s) => fetchVenueHealth(s));
  const health = new Map((data?.venues ?? []).map((v) => [v.venueId, v]));
  const connected = VENUES.filter((v) => health.get(v.id)?.status === 'healthy').length;

  return (
    <section className="card-flush section">
      <div className="card-title" style={{ color: 'var(--text)', fontSize: '0.78rem' }}>
        <span style={{ display: 'inline-flex', alignItems: 'center', gap: '0.35rem' }}>
          Venues
          <InfoTip text="Live connection health per venue — connected (green, scanning), stale (amber), or offline (red). CEX feeds are public REST; Canton venues run in mock mode until onboarded." />
        </span>
        <span className="mono mute" style={{ fontSize: '0.72rem' }}>
          {connected}/{VENUES.length} connected
        </span>
      </div>
      <div className="venue-grid">
        {VENUES.map((def) => {
          const h = health.get(def.id);
          const status: VenueHealth['status'] = h?.status ?? 'down';
          return (
            <div className="venue-cell" key={def.id}>
              <VenueLogo def={def} />
              <span className="venue-meta">
                <span className="venue-name">{def.name}</span>
                <span className="venue-kind">{def.kind}</span>
              </span>
              <span className="venue-status" data-state={status} title={h?.lastError ?? ''}>
                <span className={status === 'healthy' ? 'pulse' : undefined} style={{ display: 'inline-flex' }}>
                  <span className={`dot ${STATUS_DOT[status]}`} />
                </span>
                {STATUS_LABEL[status]}
              </span>
            </div>
          );
        })}
      </div>
    </section>
  );
}

export function StatsPanel() {
  const stats = usePoll<Stats>((s) => fetchStats(s));
  if (!stats) {
    return (
      <Card title="Spread stats">
        <p className="empty">loading…</p>
      </Card>
    );
  }
  return (
    <Card title="Spread stats · 1h" tip="Spread activity over the last hour, plus a per-route breakdown.">
      <Kv k="total spreads" v={stats.spreads.total} />
      <Kv k="last hour" v={stats.spreads.lastHour} />
      <Kv k="avg spread" v={`${stats.spreads.avgBps} bps`} />
      <Kv k="max spread" v={`${stats.spreads.maxBps} bps`} />
      {stats.spreads.byRoute.slice(0, 4).map((r) => (
        <Kv key={r.route} k={r.route} v={r.count} dim />
      ))}
    </Card>
  );
}

export function PnlPanel() {
  const stats = usePoll<Stats>((s) => fetchStats(s));
  const pnl = stats?.pnl;
  return (
    <Card
      title="P&L · paper"
      tip="Paper P&L only. Estimated = sum of detected-spread profit at the trade size; realised fills once the proprietary executor is wired (Sprint 3+)."
    >
      <Kv k="acted trades" v={pnl?.actedCount ?? 0} />
      <Kv k="estimated" v={`$${trim(pnl?.estimatedUsd ?? '0')}`} />
      <Kv k="realized" v={`$${trim(pnl?.realizedUsd ?? '0')}`} />
      <p className="mute" style={{ fontSize: '0.72rem', marginTop: '0.5rem' }}>
        realized P&L populates once the executor reports fills (Sprint 3+).
      </p>
    </Card>
  );
}

const TRADE_STATUS_COLOR: Record<string, string> = {
  SUCCESS: 'var(--good)',
  PARTIAL: 'var(--warn, #d9a300)',
  FAILED: 'var(--bad)',
  SKIPPED: 'var(--muted, #888)',
};

export function TradeLogPanel() {
  const data = usePoll<{ trades: TradeEntry[] }>((s) => fetchTrades(20, s));
  const trades = data?.trades ?? [];
  return (
    <section className="section">
      <h2 className="h-section">Trade log</h2>
      {trades.length === 0 ? (
        <p className="empty">no trades recorded yet</p>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th>time</th>
                <th>pair</th>
                <th>route</th>
                <th>status</th>
                <th className="num">P&amp;L (USDT)</th>
              </tr>
            </thead>
            <tbody>
              {trades.map((t) => (
                <tr key={t.id}>
                  <td>{new Date(t.ts).toLocaleTimeString()}</td>
                  <td>{t.ticker}</td>
                  <td className="mute">
                    {t.buyVenueId} → {t.sellVenueId}
                  </td>
                  <td style={{ color: TRADE_STATUS_COLOR[t.status] ?? 'inherit', fontWeight: 600 }}>{t.status}</td>
                  <td className="num">{t.profitUsdtDiff ? trim(t.profitUsdtDiff, 2) : '·'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

export function InventoryPanel() {
  const data = usePoll<{ holdings: Holding[] }>((s) => fetchInventory(s));
  const holdings = data?.holdings ?? [];
  return (
    <section className="section">
      <h2 className="h-section">Inventory</h2>
      {holdings.length === 0 ? (
        <p className="empty">no inventory snapshots yet</p>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th>venue</th>
                <th>asset</th>
                <th className="num">free</th>
                <th className="num">locked</th>
              </tr>
            </thead>
            <tbody>
              {holdings.map((h) => (
                <tr key={`${h.venueId}:${h.asset}`}>
                  <td>{h.venueId}</td>
                  <td>{h.asset}</td>
                  <td className="num">{trim(h.free)}</td>
                  <td className="num">{trim(h.locked)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

export function AuditLogPanel() {
  const data = usePoll<{ entries: AuditEntry[] }>((s) => fetchAuditLog(20, s));
  const entries = data?.entries ?? [];
  return (
    <section className="section">
      <h2 className="h-section">Audit log</h2>
      {entries.length === 0 ? (
        <p className="empty">no operator actions recorded yet</p>
      ) : (
        <div className="table-wrap">
          <table className="data">
            <thead>
              <tr>
                <th>time</th>
                <th>actor</th>
                <th>action</th>
                <th>payload</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((e) => (
                <tr key={e.id}>
                  <td>{new Date(e.ts).toLocaleTimeString()}</td>
                  <td>{e.actor}</td>
                  <td>{e.action}</td>
                  <td className="mute">
                    {e.payload && Object.keys(e.payload as object).length > 0
                      ? JSON.stringify(e.payload)
                      : '·'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

// ── shared bits ─────────────────────────────────────────────────────────────

export function Card({
  title,
  action,
  tip,
  children,
}: {
  title: string;
  action?: ReactNode;
  tip?: string;
  children: ReactNode;
}) {
  return (
    <div className="card">
      <div className="card-title">
        <span style={{ display: 'inline-flex', alignItems: 'center', gap: '0.35rem' }}>
          {title}
          {tip ? <InfoTip text={tip} /> : null}
        </span>
        {action}
      </div>
      {children}
    </div>
  );
}

export function Kv({ k, v, dim }: { k: string; v: string | number; dim?: boolean }) {
  return (
    <div className="kv" style={dim ? { fontSize: '0.78rem', opacity: 0.8 } : undefined}>
      <span>{k}</span>
      <span>{v}</span>
    </div>
  );
}

export function trim(raw: string, places = 4): string {
  const n = Number(raw);
  if (!Number.isFinite(n)) return raw;
  return n.toFixed(places).replace(/\.?0+$/, '') || '0';
}
