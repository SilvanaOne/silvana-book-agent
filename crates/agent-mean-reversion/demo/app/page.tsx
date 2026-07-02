"use client";

import { useCallback, useEffect, useState } from "react";
import { MrForm, type FormValues } from "./components/MrForm";
import { MrChart } from "./components/MrChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { MrState } from "@/lib/mr-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  mr: MrState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ mr: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/mr/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      emaWindow: v.emaWindow,
      deviationPct: v.deviationPct,
      quantity: v.quantity,
      warmupSamples: v.warmupSamples,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/mr/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/mr/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/mr/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.mr?.status === "monitoring";
  const hasMr = snap.mr !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid mr={snap.mr} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Mean-Reversion setup</h2>
                  <MrForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools mr={snap.mr} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · EMA · bands · signals · <span className="demobadge">DEMO</span></h2>
                  <MrChart ticks={snap.ticks} mr={snap.mr} />
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
              <p><strong>agent-mean-reversion</strong> — bets on the snap-back when price diverges from its own EMA on Canton. On every poll it updates <span className="mono">EMA(N)</span> from the mid and, once past the <span className="mono">warmup</span>, computes <span className="mono">diff = (mid − ema) / ema</span>. If <span className="mono">diff &gt; +threshold</span> it places one OFFER at the EMA (price too high, expect it to fall). If <span className="mono">diff &lt; −threshold</span> it places one BID at the EMA. Only one open order per side at a time — no stacking.</p>
              <p>The placement engine (<span className="mono">lib/mr-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-mean-reversion/src/main.rs</span>. Prices are a GBM random walk — no real orders sent, and fills are simulated whenever the mid crosses a resting order.</p>
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
