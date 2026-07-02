"use client";

import { useCallback, useEffect, useState } from "react";
import { RiskAlertForm, type FormValues } from "./components/RiskAlertForm";
import { RiskAlertChart } from "./components/RiskAlertChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { RiskAlertState } from "@/lib/riskalert-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  riskalert: RiskAlertState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ riskalert: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/risk-alert/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      maxOpenOrders: v.maxOpenOrders,
      maxFailedSettlements: v.maxFailedSettlements,
      maxOpenNotional: v.maxOpenNotional,
      checkIntervalSecs: v.checkIntervalSecs,
      orderGrowthPerTick: v.orderGrowthPerTick,
      notionalGrowthPerTick: v.notionalGrowthPerTick,
      failureRatePerTick: v.failureRatePerTick,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/risk-alert/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/risk-alert/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/risk-alert/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.riskalert?.status === "monitoring";
  const hasMr = snap.riskalert !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid riskalert={snap.riskalert} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Risk Alert setup</h2>
                  <RiskAlertForm disabled={running} onStart={start} />
                  {hasMr && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools riskalert={snap.riskalert} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Threshold gauges &amp; alerts timeline · <span className="demobadge">DEMO</span></h2>
                  <RiskAlertChart ticks={snap.ticks} riskalert={snap.riskalert} />
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
              <p><strong>agent-risk-alert</strong> — a passive threshold monitor on Canton. On every poll it samples the party&apos;s live state and checks three thresholds: <span className="mono">open_orders</span>, <span className="mono">failed_settlements</span>, and <span className="mono">open_notional</span> (sum of <span className="mono">price × remaining_qty</span> across active orders). Any threshold breach emits an <span className="mono">ALERT</span> event to the configured sinks. When the value returns below the limit, an <span className="mono">OK</span> event clears it. This agent NEVER cancels orders — pair it with <span className="mono">agent-killswitch</span> or <span className="mono">agent-circuit-breaker</span> if you also want enforcement.</p>
              <p>The engine (<span className="mono">lib/riskalert-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-risk-alert/src/main.rs</span>. Load values in this demo grow via a simple accumulator so you can watch each gauge cross its limit in real time.</p>
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
