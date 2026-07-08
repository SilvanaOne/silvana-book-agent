"use client";

import { useCallback, useEffect, useState } from "react";
import { FairValueForm, type FormValues } from "./components/FairValueForm";
import { FairValueChart } from "./components/FairValueChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { FairValueState } from "@/lib/fairvalue-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  fairvalue: FairValueState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ fairvalue: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/fair-value-multi-source/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      sources: v.sources,
      method: v.method,
      pollSecs: v.pollSecs,
      sourceNoisePct: v.sourceNoisePct,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/fair-value-multi-source/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/fair-value-multi-source/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/fair-value-multi-source/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.fairvalue?.status === "monitoring";
  const hasFv = snap.fairvalue !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid fairvalue={snap.fairvalue} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Fair Value setup</h2>
                  <FairValueForm disabled={running} onStart={start} />
                  {hasFv && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools fairvalue={snap.fairvalue} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Sources · true mid · fair value · <span className="demobadge">DEMO</span></h2>
                  <FairValueChart ticks={snap.ticks} fairvalue={snap.fairvalue} />
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
              <p><strong>agent-fair-value-multi-source</strong> — aggregates multi-source prices into a single fair value per market on Canton. On every poll it fetches <span className="mono">GetPrice</span> from each configured source (<span className="mono">binance_spot</span>, <span className="mono">bybit</span>, <span className="mono">coingecko</span>, …), then reduces them via <span className="mono">median</span>, <span className="mono">mean</span> or <span className="mono">trimmed-mean</span> (drops the highest and lowest, averages the rest) and emits one record per market per poll.</p>
              <p>The aggregation engine (<span className="mono">lib/fairvalue-engine.ts</span>) mirrors <span className="mono">crates/agent-fair-value-multi-source/src/main.rs</span>. In this demo the "true" mid follows a GBM random walk and each simulated source is that mid perturbed by an independent gaussian noise sample — the aggregate line tracks the truth even when individual feeds diverge.</p>
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
