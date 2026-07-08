"use client";

import { useCallback, useEffect, useState } from "react";
import { SmartAllocForm, type FormValues } from "./components/SmartAllocForm";
import { SmartAllocChart } from "./components/SmartAllocChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { SmartAllocState } from "@/lib/smartalloc-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  agent: SmartAllocState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.005 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/smart-allocation-buckets/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      policy: v.policy,
      bucketThresholdPct: v.bucketThresholdPct,
      rebalanceFraction: v.rebalanceFraction,
      checkIntervalSecs: v.checkIntervalSecs,
      balanceDriftPerCycle: v.balanceDriftPerCycle,
    };
    const r = await fetch("/api/smart-allocation-buckets/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/smart-allocation-buckets/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/smart-allocation-buckets/reset", { method: "POST" }); refresh(); };
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
                  <h2>Allocation setup</h2>
                  <SmartAllocForm disabled={running} onStart={start} />
                  {hasAgent && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools agent={snap.agent} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Buckets · target vs current · <span className="demobadge">DEMO</span></h2>
                  <SmartAllocChart agent={snap.agent} />
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
              <p><strong>agent-smart-allocation-buckets</strong> is a two-level portfolio allocator: instruments are grouped into <em>buckets</em> (e.g. &quot;stables&quot;, &quot;btc-theme&quot;, &quot;eth-theme&quot;), each bucket has a portfolio-level target weight, and inside a bucket each instrument has its own within-bucket weight. Every cycle the agent values instruments at their market mids, computes each bucket&apos;s current weight, and if a bucket&apos;s <span className="mono">|current − target|</span> exceeds <span className="mono">bucketThresholdPct</span>, places rebalance orders (BID for underweight buckets, OFFER for overweight) spread across the bucket&apos;s instruments by their within-bucket weights.</p>
              <p>Bucket weights <em>and</em> within-bucket weights are both normalised on load so operators can specify raw shares (e.g. <span className="mono">weight=0.5</span>) without worrying about them summing to 1.0. Try shocking the driver market with the demo tools — you&apos;ll see one bucket&apos;s bar swing outside the ±threshold band and rebalance legs fire on the next cycle. The engine (<span className="mono">lib/smartalloc-engine.ts</span>) mirrors <span className="mono">crates/agent-smart-allocation-buckets/src/main.rs</span> (<span className="mono">fn alloc_loop</span>).</p>
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
