"use client";

import { useCallback, useEffect, useState } from "react";
import { TargetAllocationForm, type FormValues } from "./components/TargetAllocationForm";
import { TargetAllocationChart } from "./components/TargetAllocationChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { TargetAllocationState } from "@/lib/targetallocation-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  targetallocation: TargetAllocationState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ targetallocation: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/target-allocation/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      targets: v.targets,
      thresholdQuote: v.thresholdQuote,
      rebalanceFraction: v.rebalanceFraction,
      checkIntervalSecs: v.checkIntervalSecs,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/target-allocation/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/target-allocation/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/target-allocation/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.targetallocation?.status === "monitoring";
  const hasTA = snap.targetallocation !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid targetallocation={snap.targetallocation} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Target Allocation setup</h2>
                  <TargetAllocationForm disabled={running} onStart={start} />
                  {hasTA && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools targetallocation={snap.targetallocation} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Current vs target · rebalance orders · <span className="demobadge">DEMO</span></h2>
                  <TargetAllocationChart ticks={snap.ticks} targetallocation={snap.targetallocation} />
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
              <p><strong>agent-target-allocation</strong> — keeps each configured instrument valued at its absolute quote-currency target. Sibling of <span className="mono">agent-portfolio-rebalancing</span> parameterised by <em>absolute</em> quote value per instrument rather than percentage weights (e.g. hold <span className="mono">10 000 USDC of Amulet</span> and <span className="mono">20 000 CC of CBTC</span> at any time).</p>
              <p>Each check-interval the agent values every position at its live market mid (<span className="mono">balance × price</span>), compares to the target quote value, and places a single rebalance order per (instrument, direction) when the absolute deviation exceeds <span className="mono">--threshold-quote</span>. Order size closes <span className="mono">--rebalance-fraction</span> of the gap: <span className="mono">qty = |deviation| × fraction / price</span>. Under-target places a <span className="mono">BID</span>, over-target places an <span className="mono">OFFER</span>. No stacking — one open order per direction.</p>
              <p>The placement engine (<span className="mono">lib/targetallocation-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-target-allocation/src/main.rs</span>. Prices are a GBM random walk — no real orders sent, and fills are simulated whenever the market mid crosses a resting order.</p>
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
