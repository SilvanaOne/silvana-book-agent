"use client";

import { useCallback, useEffect, useState } from "react";
import { KillswitchForm, type FormValues } from "./components/KillswitchForm";
import { KillswitchChart } from "./components/KillswitchChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { KillswitchState } from "@/lib/killswitch-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  killswitch: KillswitchState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ killswitch: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");
  const [panicBusy, setPanicBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/killswitch/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      maxOpenOrders: Number(v.maxOpenOrders),
      maxFailedSettlements: Number(v.maxFailedSettlements),
      checkIntervalSecs: Number(v.checkIntervalSecs),
      orderGrowthPerTick: Number(v.orderGrowthPerTick),
      failureRatePerTick: Number(v.failureRatePerTick),
      startingOpenOrders: Number(v.startingOpenOrders),
      startingFailed: Number(v.startingFailed),
    };
    const r = await fetch("/api/killswitch/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/killswitch/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/killswitch/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };
  const doPanic = async () => {
    setPanicBusy(true);
    try { await fetch("/api/killswitch/panic", { method: "POST" }); await refresh(); }
    finally { setPanicBusy(false); }
  };

  const running = snap.killswitch?.status === "monitoring";
  const tripped = snap.killswitch?.status === "tripped";
  const hasKs = snap.killswitch !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid killswitch={snap.killswitch} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Killswitch setup</h2>
                  <KillswitchForm disabled={running || tripped} onStart={start} />
                  {hasKs && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Panic</h2>
                  <div style={{ fontSize: 13, color: "var(--text-faint)", marginBottom: 10, lineHeight: 1.6 }}>
                    Immediate cancel-all. Aborts the monitor loop and cancels every open order the agent has now. Corresponds to <span className="mono">agent-killswitch panic</span>.
                  </div>
                  <button
                    onClick={doPanic}
                    disabled={!hasKs || tripped || panicBusy}
                    style={{
                      width: "100%",
                      padding: "14px 16px",
                      background: tripped ? "#3a1414" : "var(--neg)",
                      color: "#fff",
                      border: "1px solid var(--neg)",
                      borderRadius: 6,
                      fontSize: 15,
                      fontWeight: 700,
                      letterSpacing: 1,
                      cursor: !hasKs || tripped || panicBusy ? "not-allowed" : "pointer",
                      opacity: !hasKs || tripped ? 0.55 : 1,
                    }}
                  >
                    {panicBusy ? "TRIPPING…" : tripped ? "TRIPPED" : "PANIC — cancel all now"}
                  </button>
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools killswitch={snap.killswitch} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Open orders · failed settlements · thresholds · <span className="demobadge">DEMO</span></h2>
                  <KillswitchChart ticks={snap.ticks} killswitch={snap.killswitch} />
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
              <p><strong>agent-killswitch</strong> — emergency stop for the orderbook agent. Two modes: <span className="mono">panic</span> is one-shot (cancel every active order right now and exit); <span className="mono">run</span> polls a health condition on a schedule and TRIPS when it fires — cancelling everything and exiting with code <span className="mono">2</span> so a supervisor can take over.</p>
              <p>Trip conditions supported: <span className="mono">--max-open-orders</span> (active order count) and <span className="mono">--max-failed-settlements</span> (failed settlement proposals). Provide at least one.</p>
              <p>This demo simulates the monitor loop: open orders grow by a configurable amount each tick and failed settlements arrive with a configurable probability. Every <span className="mono">check_interval</span> the guard evaluates both counters. When either exceeds its limit the killswitch TRIPS — the red X on the chart, event log entries mirroring the Rust <span className="mono">error!</span> output, and the number of orders cancelled. The <span className="mono">PANIC</span> button on the dashboard mirrors the <span className="mono">panic</span> subcommand.</p>
              <p>The state machine (<span className="mono">lib/killswitch-engine.ts</span>) mirrors the Rust logic in <span className="mono">crates/agent-killswitch/src/main.rs</span> (<span className="mono">run_monitor</span>, <span className="mono">evaluate</span>, <span className="mono">cancel_all</span>). No real orders are placed — this is a teaching model, not a backtest.</p>
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
