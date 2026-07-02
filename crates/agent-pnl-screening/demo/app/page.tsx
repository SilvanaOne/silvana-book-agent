"use client";

import { useCallback, useEffect, useState } from "react";
import { PnlScreeningForm, type FormValues } from "./components/PnlScreeningForm";
import { PnlScreeningChart } from "./components/PnlScreeningChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { PnlScreeningState } from "@/lib/pnlscreening-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  pnlscreening: PnlScreeningState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ pnlscreening: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/pnl-screening/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      snapshotIntervalSecs: v.snapshotIntervalSecs,
      tradeArrivalPerTick: v.tradeArrivalPerTick,
      avgTradeQty: v.avgTradeQty,
      startingPrice: v.startingPrice,
      startingPosition: v.startingPosition,
      startingCostBasis: v.startingCostBasis,
    };
    const r = await fetch("/api/pnl-screening/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/pnl-screening/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/pnl-screening/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.pnlscreening?.status === "monitoring";
  const hasMr = snap.pnlscreening !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid pnlscreening={snap.pnlscreening} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>PnL Screening setup</h2>
                  <PnlScreeningForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools pnlscreening={snap.pnlscreening} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · trades · realized/unrealized PnL · <span className="demobadge">DEMO</span></h2>
                  <PnlScreeningChart ticks={snap.ticks} pnlscreening={snap.pnlscreening} />
                </div>
                <div className="card">
                  <h2>Recent events</h2>
                  <EventsLog events={snap.events.slice(-8)} />
                </div>
              </div>
            </div>
          </>
        )}

        {tab === "events" && (
          <div className="card">
            <h2>Events log</h2>
            <EventsLog events={snap.events} />
          </div>
        )}

        {tab === "docs" && (
          <div className="card">
            <h2>About this demo</h2>
            <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
              <p><strong>agent-pnl-screening</strong> — read-only observer of the party&apos;s settlement stream on Canton. For every settled fill it updates the per-market <span className="mono">position</span> and <span className="mono">weighted-avg cost basis</span>, accrues <span className="mono">realized PnL</span> when a sell reduces a long position (mirror for shorts), and every <span className="mono">--snapshot-secs</span> recomputes <span className="mono">unrealized PnL = position × (mid − cost)</span> using the live mid price. No orders, no ledger writes.</p>
              <p>This demo (<span className="mono">lib/pnlscreening-engine.ts</span>) mirrors the same math in TypeScript but synthesizes fills locally: a Poisson-ish arrival at <span className="mono">tradeArrivalPerTick</span> generates BUY/SELL trades against a GBM mid, then the same WAVG + realized-PnL rules run on top. Interval snapshots are emitted just like the real agent&apos;s <span className="mono">pnl.snapshot</span> records.</p>
              <ul style={{ paddingLeft: 18 }}>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://github.com/SilvanaOne/silvana-book-agent" target="_blank" rel="noopener">github: silvana-book-agent</a></li>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://docs.silvana.one" target="_blank" rel="noopener">docs.silvana.one</a></li>
                <li><a className="mono" style={{ color: "var(--accent)" }} href="https://canton.network" target="_blank" rel="noopener">canton.network</a></li>
              </ul>
            </div>
          </div>
        )}
      </div>
      <Footer />
    </>
  );
}
