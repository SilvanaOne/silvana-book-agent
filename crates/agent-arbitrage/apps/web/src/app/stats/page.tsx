'use client';

import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { useAuth, AUTH_DISABLED } from '@/lib/auth';
import { Brand, NavTabs } from '@/components/chrome';
import { ThemeToggle } from '@/components/ThemeToggle';
import { InfoTip } from '@/components/InfoTip';
import { HBarChart, Histogram, Donut, type Datum } from '@/components/charts';
import { VENUES, venueSortKey } from '@/components/panels';
import { fetchProfitability, type Profitability } from '@/lib/api';
import { demoProfitability } from '@/lib/demo';
import { DEMO_COIN_SHARE, UI_TOKENS } from '@/lib/tokens';

const POLL_MS = 6000;

function venueMeta(id: string): { name: string; color: string } {
  const v = VENUES.find((x) => x.id === id);
  return { name: v?.name ?? id, color: v?.color ?? '#8a8a97' };
}

export default function StatsPage() {
  const router = useRouter();
  const { session, bootstrapping } = useAuth();
  const [data, setData] = useState<Profitability | null>(null);

  useEffect(() => {
    if (!AUTH_DISABLED && !bootstrapping && !session) router.replace('/login');
  }, [bootstrapping, session, router]);

  useEffect(() => {
    if (!session) return;
    const ac = new AbortController();
    const tick = async (): Promise<void> => {
      try {
        const d = await fetchProfitability(ac.signal);
        if (!ac.signal.aborted) setData(d);
      } catch {
        /* keep last good */
      }
    };
    void tick();
    const id = setInterval(() => void tick(), POLL_MS);
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

  // While the agent isn't yet hooked up to live exchange APIs / on-chain
  // transactions, fall back to a hand-tuned demo set so the charts are never
  // blank. The moment real spreads land, this swap reverses automatically.
  const isDemo = !data || data.totals.count === 0;
  const effective = isDemo ? demoProfitability() : data!;

  // Build a row for EVERY venue in the catalogue (Silvana, Temple, Cantex,
  // OneSwap, Bybit, KuCoin) so the CEX venues don't disappear from the charts
  // when they have zero profitable spreads — that was making the widgets look
  // DEX-only. Real counts win when present; otherwise the venue shows with 0.
  const byVenueMap = new Map(effective.byVenue.map((v) => [v.venueId, v]));
  const fullByVenue = VENUES.map(
    (def) =>
      byVenueMap.get(def.id) ?? {
        venueId: def.id,
        count: 0,
        buyCount: 0,
        sellCount: 0,
        estProfitUsd: '0',
      },
  );
  // Silvana always first; order within the rest follows the catalogue.
  const byVenueSorted = [...fullByVenue].sort(
    (a, b) => venueSortKey(a.venueId) - venueSortKey(b.venueId),
  );

  const byVenueBars: Datum[] = byVenueSorted.map((v) => {
    const m = venueMeta(v.venueId);
    return { label: m.name, value: v.count, color: m.color, sub: `buy ${v.buyCount} · sell ${v.sellCount}` };
  });
  const venueShare: Datum[] = byVenueSorted.map((v) => {
    const m = venueMeta(v.venueId);
    return { label: m.name, value: v.count, color: m.color };
  });
  const sizeBars: Datum[] = effective.bySize.map((b) => ({ label: b.label, value: b.count }));

  const totals = effective.totals;

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

        <h1 className="h-page" style={{ marginBottom: '0.3rem', display: 'inline-flex', alignItems: 'center', gap: '0.55rem', flexWrap: 'wrap' }}>
          <span><span className="accent-text">Stats</span> — profitability</span>
          {isDemo ? <span className="badge" data-demo>demo · placeholder</span> : null}
        </h1>
        <p className="dim" style={{ marginTop: 0, fontSize: '0.9rem' }}>
          Where the agent finds edge: spread volume by venue and the size distribution of every
          opportunity caught. Estimated P&amp;L is paper until the executor reports fills.
          {isDemo ? ` Charts below are demo placeholders (tokens: ${UI_TOKENS.join(', ')}) until the agent is wired to live exchange APIs and on-chain transactions.` : ''}
        </p>

        <section className="grid grid-auto" style={{ marginTop: '1.25rem' }}>
          <StatCard
            label="Spreads caught"
            value={totals ? String(totals.count) : '—'}
            tip="Total spread opportunities the scanner has detected and persisted (all-time, paper mode)."
          />
          <StatCard
            label="Est. profit"
            value={totals ? `$${Number(totals.estProfitUsd).toFixed(2)}` : '—'}
            accent
            tip="Sum of estimated profit across all detected spreads at the configured trade size. Not realised P&L."
          />
          <StatCard
            label="Avg spread"
            value={totals ? `${totals.avgBps} bps` : '—'}
            tip="Mean spread width across all detected opportunities."
          />
          <StatCard
            label="Peak spread"
            value={totals ? `${totals.maxBps} bps` : '—'}
            tip="Widest single spread detected."
          />
        </section>

        <section className="grid grid-auto section">
          <div className="card-flush">
            <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
              Profitable spreads by venue
              <InfoTip text="How many detected spreads each venue took part in, as the buy or the sell leg. Bars are coloured by venue brand." />
            </h2>
            <HBarChart data={byVenueBars} />
          </div>

          <div className="card-flush">
            <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
              Share by venue
              <InfoTip text="Each venue's share of total spread participation." />
            </h2>
            <Donut data={venueShare} centerLabel="spreads" />
          </div>

          <div className="card-flush">
            <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
              Spread Share by Coin
              <span className="badge" data-demo>demo</span>
              <InfoTip text="Share of caught spreads attributed to each asset in the watch list (CC, CBTC, CETH, USDC, USDCx). Placeholder figures until the agent is wired to live exchange APIs and on-chain transactions." />
            </h2>
            <Donut data={[...DEMO_COIN_SHARE]} centerLabel="spreads" />
          </div>
        </section>

        <section className="card-flush section">
          <h2 className="h-section" style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
            Spread size distribution
            <InfoTip text="Count of caught spreads bucketed by width in basis points, from small (&lt;50 bps) to large (500+ bps)." />
          </h2>
          <Histogram data={sizeBars} />
        </section>
      </div>
    </div>
  );
}

function StatCard({
  label,
  value,
  tip,
  accent,
}: {
  label: string;
  value: string;
  tip: string;
  accent?: boolean;
}) {
  return (
    <div className="card">
      <div className="card-title">
        <span>{label}</span>
        <InfoTip text={tip} />
      </div>
      <div
        className="mono"
        style={{
          fontFamily: 'var(--font-display)',
          fontWeight: 700,
          fontSize: '1.6rem',
          letterSpacing: '-0.02em',
          color: accent ? 'transparent' : 'var(--text)',
          background: accent ? 'var(--accent-grad)' : undefined,
          WebkitBackgroundClip: accent ? 'text' : undefined,
          backgroundClip: accent ? 'text' : undefined,
        }}
      >
        {value}
      </div>
    </div>
  );
}
