"use client";

import { useCallback, useEffect, useState } from "react";
import { InvRiskForm, type FormValues } from "./components/InvRiskForm";
import { InvRiskChart } from "./components/InvRiskChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { InvRiskState } from "@/lib/invrisk-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  agent: InvRiskState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.005 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/inventory-risk-auto-hedge/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      instrument: v.instrument,
      hedgeMarket: v.hedgeMarket,
      target: v.target,
      softTolerance: v.softTolerance,
      hardTolerance: v.hardTolerance,
      startingBalance: v.startingBalance,
      startingPrice: v.startingPrice,
      autoHedge: v.autoHedge,
      checkIntervalSecs: v.checkIntervalSecs,
      driftPerCycle: v.driftPerCycle,
    };
    const r = await fetch("/api/inventory-risk-auto-hedge/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/inventory-risk-auto-hedge/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/inventory-risk-auto-hedge/reset", { method: "POST" }); refresh(); };
  const nudge = async (delta: number) => { await fetch("/api/inventory-risk-auto-hedge/nudge", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ delta }) }); };
  const autoHedge = async () => { await fetch("/api/inventory-risk-auto-hedge/autohedge", { method: "POST" }); refresh(); };
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
                  <h2>Band setup</h2>
                  <InvRiskForm disabled={running} onStart={start} />
                  {hasAgent && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools agent={snap.agent} onNudge={nudge} onToggleAutoHedge={autoHedge} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Balance × soft/hard bands · <span className="demobadge">DEMO</span></h2>
                  <InvRiskChart agent={snap.agent} />
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
              <p><strong>agent-inventory-risk-auto-hedge</strong> watches the unlocked balance of an instrument against a target band with <em>two</em> layers: soft and hard. Inside <span className="mono">±soft_tolerance</span> the balance is OK. Beyond soft, an <span className="mono">inventory.risk</span> signal is emitted (suggested hedge direction + size) but nothing is placed. Beyond <span className="mono">±hard_tolerance</span>, if <span className="mono">--auto-hedge</span> is set, the agent submits an opposite-side limit order on the hedge market to bring the balance back to target.</p>
              <p>Unlike <span className="mono">agent-hedging</span> (which fires on every deviation), the two-band shape lets an unrelated strategy work itself out under normal drift and only intervenes at the outer edge. This demo simulates that with a walking balance driven by "strategy pressure" — try the ±hard button and watch the hedge fill over the next few ticks. The engine (<span className="mono">lib/invrisk-engine.ts</span>) mirrors <span className="mono">crates/agent-inventory-risk-auto-hedge/src/main.rs</span> (<span className="mono">fn inv_risk_loop</span>).</p>
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
