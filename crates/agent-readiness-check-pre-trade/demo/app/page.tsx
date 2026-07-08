"use client";

import { useCallback, useEffect, useState } from "react";
import { ReadinessCheckForm, type FormValues } from "./components/ReadinessCheckForm";
import { ReadinessCheckChart } from "./components/ReadinessCheckChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ReadinessCheckState } from "@/lib/readinesscheck-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  readinesscheck: ReadinessCheckState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ readinesscheck: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/readiness-check-pre-trade/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      requiredBalances: v.requiredBalances,
      maxFailedSettlements: v.maxFailedSettlements,
      maxPendingSettlements: v.maxPendingSettlements,
      requirePreapproval: v.requirePreapproval,
      checkIntervalSecs: v.checkIntervalSecs,
      driftEnabled: v.driftEnabled,
      startingPrice: v.startingPrice,
      initialFailed: v.initialFailed,
      initialPending: v.initialPending,
    };
    const r = await fetch("/api/readiness-check-pre-trade/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/readiness-check-pre-trade/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/readiness-check-pre-trade/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.readinesscheck?.status === "monitoring";
  const hasRc = snap.readinesscheck !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid readinesscheck={snap.readinesscheck} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Readiness Check setup</h2>
                  <ReadinessCheckForm disabled={running} onStart={start} />
                  {hasRc && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools readinesscheck={snap.readinesscheck} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Checklist · ready timeline · <span className="demobadge">DEMO</span></h2>
                  <ReadinessCheckChart ticks={snap.ticks} readinesscheck={snap.readinesscheck} />
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
              <p><strong>agent-readiness-check-pre-trade</strong> — pre-trade gate. Before launching a production trading agent, it verifies the party is provisioned: required per-instrument unlocked balances are present, no stuck/failed settlements above configurable limits, and (optionally) at least one <span className="mono">TransferPreapproval</span> contract exists.</p>
              <p>This demo re-runs the check every <span className="mono">checkIntervalSecs</span> against a synthetic party state that drifts each tick — so operators can watch READY / NOT READY transitions in real time. Each cycle emits a list of individual pass/fail checks and an overall verdict.</p>
              <p>The rule logic (<span className="mono">lib/readinesscheck-engine.ts</span>) mirrors the Rust <span className="mono">fn check</span> in <span className="mono">crates/agent-readiness-check-pre-trade/src/main.rs</span>. No real ledger calls are made — this is a UI teaching model of the gate, not a live check.</p>
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
