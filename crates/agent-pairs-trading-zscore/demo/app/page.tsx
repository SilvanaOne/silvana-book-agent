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
      const r = await fetch("/api/pairs-trading-zscore/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      marketA: v.marketA,
      marketB: v.marketB,
      window: v.window,
      entryZ: v.entryZ,
      exitZ: v.exitZ,
      quantityA: v.quantityA,
      quantityB: v.quantityB,
      warmupSamples: v.warmupSamples,
      startingPriceA: v.startingPriceA,
      startingPriceB: v.startingPriceB,
    };
    const r = await fetch("/api/pairs-trading-zscore/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/pairs-trading-zscore/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/pairs-trading-zscore/reset", { method: "POST" }); refresh(); };
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
                  <h2>Pairs Trading (Z-Score) setup</h2>
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
                  <h2>Leg A · Leg B · ratio · z-bands · signals · <span className="demobadge">DEMO</span></h2>
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
              <p><strong>agent-pairs-trading-zscore</strong> — a rolling-window stat-arb variant. Instead of comparing the live ratio to a fixed target, it maintains the last <span className="mono">window</span> samples of <span className="mono">ratio = mid_A / mid_B</span> and evaluates the z-score:</p>
              <pre className="mono" style={{ background: "#161822", padding: 10, borderRadius: 6 }}>
{`μ = mean(window)
σ = sample_stddev(window)
z = (ratio − μ) / σ`}
              </pre>
              <ul style={{ paddingLeft: 18 }}>
                <li><span className="mono">z &gt; +entry_z</span> — A is relatively expensive → OFFER on A, BID on B</li>
                <li><span className="mono">z &lt; −entry_z</span> — A is relatively cheap → BID on A, OFFER on B</li>
                <li><span className="mono">|z| ≤ exit_z</span> — dead / exit band, clears the position flag; no new entries</li>
              </ul>
              <p>At most one open order per leg in the intended direction — the agent does not stack and does not actively manage exits (existing orders live/expire naturally). Warmup requires the buffer to reach <span className="mono">warmup_samples</span> (defaults to <span className="mono">window</span>) and σ &gt; 0 with at least 3 samples before any signal is emitted. The placement engine (<span className="mono">lib/pairstrading-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-pairs-trading-zscore/src/main.rs</span>. Leg A follows the demo&apos;s master GBM walk; leg B walks independently so the ratio actually moves. No real orders are sent — fills are simulated whenever a leg&apos;s mid crosses a resting order.</p>
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
