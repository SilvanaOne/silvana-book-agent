"use client";

import { useCallback, useEffect, useState } from "react";
import { YieldRotationForm, type FormValues } from "./components/YieldRotationForm";
import { YieldRotationChart, ScoreTimeline } from "./components/YieldRotationChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { YieldRotationState } from "@/lib/yieldrotation-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  yieldrotation: YieldRotationState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ yieldrotation: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.005 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/yield-rotation-carry-ranking/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      markets: v.markets,
      wChange: v.wChange,
      wVolume: v.wVolume,
      wSpread: v.wSpread,
      pollSecs: v.pollSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/yield-rotation-carry-ranking/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/yield-rotation-carry-ranking/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/yield-rotation-carry-ranking/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.yieldrotation?.status === "monitoring";
  const hasYr = snap.yieldrotation !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid yieldrotation={snap.yieldrotation} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Yield Rotation setup</h2>
                  <YieldRotationForm disabled={running} onStart={start} />
                  {hasYr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools yieldrotation={snap.yieldrotation} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Score ranking · top market highlighted · <span className="demobadge">DEMO</span></h2>
                  <YieldRotationChart yieldrotation={snap.yieldrotation} />
                  <div style={{ marginTop: 8, fontSize: 12, color: "var(--text-faint)" }}>Score history &amp; rotation markers:</div>
                  <ScoreTimeline yieldrotation={snap.yieldrotation} />
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
              <p><strong>agent-yield-rotation-carry-ranking</strong> — ranks a basket of markets by a simple yield score and emits a rotation signal whenever the top-ranked market changes. Score is <span className="mono">w_change × change_24h_pct + w_volume × log10(volume_24h) − w_spread × spread_pct</span>, with configurable weights per factor.</p>
              <p>Every <span className="mono">poll_secs</span> the engine publishes the full ranking. When the top market flips, a <span className="mono">yield.rotation_signal</span> is emitted so a downstream agent (e.g. <span className="mono">agent-batch-orders</span>) can rebalance capital toward the new leader. Read-only on its own — no ledger writes.</p>
              <p>The scoring engine (<span className="mono">lib/yieldrotation-engine.ts</span>) mirrors the Rust in <span className="mono">crates/agent-yield-rotation-carry-ranking/src/main.rs</span>. Per-market metrics are simulated locally so the demo runs without an orderbook connection.</p>
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
