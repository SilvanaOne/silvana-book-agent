"use client";

import { useCallback, useEffect, useState } from "react";
import { PairsTradingForm, type FormValues } from "./components/PairsTradingForm";
import { PairsTradingChart } from "./components/PairsTradingChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { PairsTradingState } from "@/lib/pairstrading-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  pairstrading: PairsTradingState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ pairstrading: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/pairs-trading/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      marketA: v.marketA,
      marketB: v.marketB,
      targetRatio: v.targetRatio,
      thresholdPct: v.thresholdPct,
      quantityA: v.quantityA,
      quantityB: v.quantityB,
      startingPriceA: v.startingPriceA,
      startingPriceB: v.startingPriceB,
    };
    const r = await fetch("/api/pairs-trading/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/pairs-trading/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/pairs-trading/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.pairstrading?.status === "monitoring";
  const hasPt = snap.pairstrading !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid pairstrading={snap.pairstrading} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Pairs Trading setup</h2>
                  <PairsTradingForm disabled={running} onStart={start} />
                  {hasPt && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools pairstrading={snap.pairstrading} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Leg A · Leg B · ratio band · signals · <span className="demobadge">DEMO</span></h2>
                  <PairsTradingChart ticks={snap.ticks} pairstrading={snap.pairstrading} />
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
              <p><strong>agent-pairs-trading</strong> — a minimal stat-arb skeleton. On every poll it fetches the mid on two markets A and B and computes <span className="mono">ratio = mid_A / mid_B</span>. When <span className="mono">|(ratio − target)/target| × 100 &gt; threshold_pct</span> the agent places <em>two</em> limit orders in opposite directions to push the ratio back toward the target:</p>
              <ul style={{ paddingLeft: 18 }}>
                <li><span className="mono">ratio &gt; target</span> — A is rich, B is cheap → OFFER on A, BID on B</li>
                <li><span className="mono">ratio &lt; target</span> — A is cheap, B is rich → BID on A, OFFER on B</li>
              </ul>
              <p>At most one open order per leg in the intended direction — the agent does not stack. The placement engine (<span className="mono">lib/pairstrading-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-pairs-trading/src/main.rs</span>. Leg A follows the demo&apos;s master GBM walk; leg B walks independently so the ratio actually moves. No real orders are sent — fills are simulated whenever a leg&apos;s mid crosses a resting order.</p>
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
