"use client";

import { useCallback, useEffect, useState } from "react";
import { VolatilityScreeningForm, type FormValues } from "./components/VolatilityScreeningForm";
import { VolatilityScreeningChart } from "./components/VolatilityScreeningChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { VolatilityScreeningState } from "@/lib/volatilityscreening-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  volatilityscreening: VolatilityScreeningState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ volatilityscreening: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/volatility-screening/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      window: v.window,
      pollSecs: v.pollSecs,
      periodsPerYear: v.periodsPerYear,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/volatility-screening/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/volatility-screening/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/volatility-screening/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.volatilityscreening?.status === "monitoring";
  const hasMr = snap.volatilityscreening !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid volatilityscreening={snap.volatilityscreening} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Volatility setup</h2>
                  <VolatilityScreeningForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools volatilityscreening={snap.volatilityscreening} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · realized vol · high-return markers · <span className="demobadge">DEMO</span></h2>
                  <VolatilityScreeningChart ticks={snap.ticks} volatilityscreening={snap.volatilityscreening} />
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
              <p><strong>agent-volatility-screening</strong> — a read-only observability agent that publishes a rolling realized-volatility estimate for a Canton market. On every tick it samples the mid, computes the log-return against the previous sample, and stores it in a bounded window of the last <span className="mono">window</span> values. On every publish cycle it emits <span className="mono">realized_vol_annualized = std(returns) × sqrt(periodsPerYear / pollSecs)</span>.</p>
              <p>The publisher (<span className="mono">lib/volatilityscreening-engine.ts</span>) mirrors the Rust logic in <span className="mono">crates/agent-volatility-screening/src/main.rs</span>. No orders are placed. Prices in the demo come from a GBM random walk — use the nudge controls or crank <span className="mono">vol / tick</span> to watch the realized-vol curve react.</p>
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
