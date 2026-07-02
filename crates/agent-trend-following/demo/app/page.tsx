"use client";

import { useCallback, useEffect, useState } from "react";
import { TrendFollowingForm, type FormValues } from "./components/TrendFollowingForm";
import { TrendFollowingChart } from "./components/TrendFollowingChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { TrendFollowingState } from "@/lib/trendfollowing-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  trendfollowing: TrendFollowingState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ trendfollowing: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/trend-following/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      fastWindow: v.fastWindow,
      slowWindow: v.slowWindow,
      quantity: v.quantity,
      warmupSamples: v.warmupSamples,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/trend-following/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/trend-following/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/trend-following/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.trendfollowing?.status === "monitoring";
  const hasTf = snap.trendfollowing !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid trendfollowing={snap.trendfollowing} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Trend Following setup</h2>
                  <TrendFollowingForm disabled={running} onStart={start} />
                  {hasTf && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools trendfollowing={snap.trendfollowing} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · fast/slow EMA · crossover signals · <span className="demobadge">DEMO</span></h2>
                  <TrendFollowingChart ticks={snap.ticks} trendfollowing={snap.trendfollowing} />
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
              <p><strong>agent-trend-following</strong> — momentum trader that rides EMA crossovers on Canton. On every poll it updates two exponential moving averages, <span className="mono">fast_ema</span> (period <span className="mono">--fast</span>) and <span className="mono">slow_ema</span> (period <span className="mono">--slow</span>), from the live mid. Once past <span className="mono">warmup</span> ticks: when <span className="mono">fast</span> crosses <strong>above</strong> <span className="mono">slow</span> it fires a bullish signal and places one BID at mid; when <span className="mono">fast</span> crosses <strong>below</strong> it fires a bearish signal and places one OFFER at mid. Only one open order per side at a time — no stacking.</p>
              <p>The placement engine (<span className="mono">lib/trendfollowing-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-trend-following/src/main.rs</span>. Prices are a GBM random walk — no real orders sent, and fills are simulated whenever the mid crosses a resting order.</p>
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
