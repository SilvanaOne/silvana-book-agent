"use client";

import { useCallback, useEffect, useState } from "react";
import { RiskMgmtForm, type FormValues } from "./components/RiskMgmtForm";
import { RiskMgmtChart } from "./components/RiskMgmtChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { RiskState } from "@/lib/riskmgmt-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  agent: RiskState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.005 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/risk-management-composite/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      markets: v.markets,
      startingPrices: v.startingPrices,
      checkIntervalSecs: v.checkIntervalSecs,
      spawnPerCycle: v.spawnPerCycle,
      enforce: v.enforce,
      maxOpenOrders: v.maxOpenOrders,
      maxOpenNotional: v.maxOpenNotional,
      maxPendingSettlements: v.maxPendingSettlements,
      maxFailedSettlements: v.maxFailedSettlements,
      perMarketMaxNotional: v.perMarketMaxNotional,
    };
    const r = await fetch("/api/risk-management-composite/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/risk-management-composite/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/risk-management-composite/reset", { method: "POST" }); refresh(); };
  const enforce = async () => { await fetch("/api/risk-management-composite/enforce", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.agent?.status === "running";
  const hasAgent = snap.agent !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid agent={snap.agent} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Policy setup</h2>
                  <RiskMgmtForm disabled={running} onStart={start} />
                  {hasAgent && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools agent={snap.agent} onJump={jump} onWalk={walk} onToggleEnforce={enforce} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Notional × limits · <span className="demobadge">DEMO</span></h2>
                  <RiskMgmtChart agent={snap.agent} />
                </div>
                <div className="card">
                  <h2>Recent events</h2>
                  <EventsLog events={snap.events.slice(-10)} />
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
              <p><strong>agent-risk-management-composite</strong> evaluates a composite TOML policy every cycle: <span className="mono">max_open_orders</span>, <span className="mono">max_open_notional</span>, <span className="mono">per_market_max_notional</span>, <span className="mono">max_pending_settlements</span>, <span className="mono">max_failed_settlements</span>. Each cycle it queries your own orders + settlement proposals, computes open notional (globally + per market), compares to limits, and emits one <span className="mono">risk.status</span> event with hit list. With <span className="mono">--enforce</span>, breaches trigger newest-first cancellations to bring the book back inside the limit. Pending/failed settlements are report-only (owned by the orderbook — see <span className="mono">agent-failure-recovery</span>).</p>
              <p>This demo simulates a small trading book: orders spawn/decay each tick, prices walk via GBM, pending settlements churn. Try tightening a limit and toggling enforce — you&apos;ll see the newest orders get cancelled to bring notional back under cap. The engine (<span className="mono">lib/riskmgmt-engine.ts</span>) mirrors <span className="mono">crates/agent-risk-management-composite/src/main.rs</span> (<span className="mono">fn evaluate</span>).</p>
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
