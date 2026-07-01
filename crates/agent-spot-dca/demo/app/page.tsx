"use client";

import { useCallback, useEffect, useState } from "react";
import { DcaForm, type FormValues } from "./components/DcaForm";
import { DcaChart } from "./components/DcaChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { DcaState } from "@/lib/dca-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  dca: DcaState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ dca: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.005 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/dca/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = { market: v.market, side: v.side,
      amountPerOrder: v.amountPerOrder, intervalSecs: v.intervalSecs,
      priceOffsetPct: v.priceOffsetPct, maxTotal: v.maxTotal === "" ? null : v.maxTotal,
      startingPrice: v.startingPrice };
    const r = await fetch("/api/dca/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/dca/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/dca/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.dca?.status === "monitoring";
  const done = snap.dca?.status === "completed";
  const hasDca = snap.dca !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid dca={snap.dca} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>DCA setup</h2>
                  <DcaForm disabled={running || done} onStart={start} />
                  {hasDca && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop DCA</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools dca={snap.dca} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Price + orders · <span className="demobadge">DEMO</span></h2>
                  <DcaChart ticks={snap.ticks} dca={snap.dca} />
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
              <p><strong>agent-spot-dca</strong> — Dollar-Cost Averaging on Canton. Every N seconds it places one limit order at mid ± offset, accumulating (or distributing) a base instrument at a controlled cadence.</p>
              <p>The placement engine (<span className="mono">lib/dca-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-spot-dca/src/main.rs</span>. Prices are a GBM random walk — no real orders sent.</p>
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
