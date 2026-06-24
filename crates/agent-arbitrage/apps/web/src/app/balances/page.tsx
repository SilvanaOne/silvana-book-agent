'use client';

import { useEffect, useMemo, useState } from 'react';
import { useRouter } from 'next/navigation';
import { useAuth, AUTH_DISABLED } from '@/lib/auth';
import { Brand, NavTabs } from '@/components/chrome';
import { ThemeToggle } from '@/components/ThemeToggle';
import { fetchBalances, type BalanceSnapshot } from '@/lib/api';

const POLL_MS = 5000;
const EMPTY: BalanceSnapshot = { updatedAt: 0, totalUsd: 0, sources: [], rows: [] };

function usd(n: number | null | undefined): string {
  if (n === null || n === undefined) return '—';
  return n.toLocaleString(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 2 });
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  if (abs >= 1) return n.toLocaleString(undefined, { maximumFractionDigits: 4 });
  return n.toPrecision(4);
}

function shortAddr(a: string | undefined): string {
  if (!a || a.length < 16) return a ?? '';
  return `${a.slice(0, 8)}…${a.slice(-6)}`;
}

export default function BalancesPage() {
  const router = useRouter();
  const { session, bootstrapping } = useAuth();
  const [snap, setSnap] = useState<BalanceSnapshot>(EMPTY);

  useEffect(() => {
    if (!AUTH_DISABLED && !bootstrapping && !session) router.replace('/login');
  }, [bootstrapping, session, router]);

  useEffect(() => {
    if (!session) return;
    const ac = new AbortController();
    const tick = async (): Promise<void> => {
      try {
        const data = await fetchBalances(ac.signal);
        if (!ac.signal.aborted) setSnap(data);
      } catch {
        /* keep last cached snapshot visible */
      }
    };
    void tick();
    const id = setInterval(() => void tick(), POLL_MS);
    return () => {
      ac.abort();
      clearInterval(id);
    };
  }, [session]);

  const bySource = useMemo(() => {
    type CoinRow = BalanceSnapshot['rows'][number];
    const map = new Map<string, { chain: string; address?: string; coins: CoinRow[] }>();
    for (const r of snap.rows) {
      const g = map.get(r.sourceId) ?? { chain: r.venueOrChain, address: r.address, coins: [] as CoinRow[] };
      if (r.address) g.address = r.address;
      if (r.asset && r.asset !== '—' && (r.free || r.locked || r.usd)) g.coins.push(r);
      map.set(r.sourceId, g);
    }
    return map;
  }, [snap.rows]);

  if (bootstrapping || !session) {
    return (
      <div className="app-bg">
        <div className="shell">
          <p className="empty">loading…</p>
        </div>
      </div>
    );
  }

  const updated =
    snap.updatedAt > 0
      ? new Date(snap.updatedAt).toLocaleString()
      : 'not refreshed yet — background sync every ~30s';

  return (
    <div className="app-bg">
      <div className="shell">
        <header className="topbar">
          <Brand />
          <div className="topbar-right">
            <ThemeToggle />
          </div>
        </header>
        <NavTabs />

        <section className="panel">
          <h1 className="page-title">Balances</h1>
          <p className="page-sub">
            Per-key holdings from Silvana Vault (same API keys). Display is always from cache — no live fetch on page load.
          </p>
          <p className="bal-meta">
            Total {usd(snap.totalUsd)} · last refresh {updated}
          </p>

          <div className="bcards">
            {snap.sources.length === 0 ? (
              <p className="empty">No keys loaded in Vault yet — add keys at vault.spcatcher.cfd</p>
            ) : (
              snap.sources.map((src) => {
                const g = bySource.get(src.id) ?? { chain: '', coins: [] };
                const kind = src.kind === 'cex' ? 'CEX' : 'CANTON';
                const addr = g.address ? ` · ${shortAddr(g.address)}` : '';
                return (
                  <div key={src.id} className="bcard">
                    <div className="bcard-h">
                      <div>
                        <span className="lab">{src.label}</span>
                        <span className="bal-pill">{kind}</span>
                      </div>
                      <span className="bcard-sub">{usd(src.totalUsd)}</span>
                    </div>
                    <div className="bcard-chain">
                      {g.chain}
                      {addr}
                    </div>
                    {g.coins.length > 0 ? (
                      g.coins.map((c) => (
                        <div key={`${c.sourceId}-${c.asset}`} className="bcoin">
                          <span className="asset">{c.asset}</span>
                          <span className="amt">
                            {fmt(c.free)}
                            {c.locked > 0 ? ` (+${fmt(c.locked)} locked)` : ''}
                          </span>
                          <span className="amt">{usd(c.usd)}</span>
                        </div>
                      ))
                    ) : src.error ? null : (
                      <div className="bcard-empty">no balances</div>
                    )}
                    {src.error ? <div className="bcard-empty bcard-err">{src.error}</div> : null}
                  </div>
                );
              })
            )}
          </div>
        </section>
      </div>
    </div>
  );
}
