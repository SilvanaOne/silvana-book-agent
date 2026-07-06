"use client";

import { useCallback, useEffect, useState } from "react";
import { AlgoForm, type FormValues } from "./components/AlgoForm";
import { AlgoChart } from "./components/AlgoChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { AlgoState } from "@/lib/algo-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = { agent: AlgoState | null; ticks: Tick[]; events: EventEntry[]; walk: { driftPerTick: number; volPerTick: number } };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.004 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/algo-order/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/algo-order/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ plan: v.plan, startingPrice: v.startingPrice, bookVolatility: v.bookVolatility }) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/algo-order/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/algo-order/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.agent?.status === "running";
  const hasAgent = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={running} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} walk={snap.walk} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Plan setup</h2>
              <AlgoForm disabled={running} onStart={start} />
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
              <h2>Plan progress · <span className="demobadge">DEMO</span></h2>
              <AlgoChart agent={snap.agent} />
            </div>
            <div className="card">
              <h2>Recent events</h2>
              <EventsLog events={snap.events.slice(-10)} />
            </div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-algo-order</strong> is a pluggable execution dispatcher. A plan lists any number of steps; each step names an algorithm and its parameters and the agent runs them sequentially against the orderbook. Three algos ship in-process: <span className="mono">twap</span> (equal time-sliced children), <span className="mono">iceberg</span> (small visible chunks, next placed only after previous clears), and <span className="mono">liquidity-seeking</span> (walk depth each cycle, size the child to the slippage budget).</p>
            <p>This demo executes a plan of up to 10 steps against a simulated book. Watch the progress bar per step fill in real-time and try nudging the mid outside an iceberg step&apos;s <span className="mono">price</span> — the iceberg stalls while later TWAP/liq-seek steps still deliver against the moving mid. The engine (<span className="mono">lib/algo-engine.ts</span>) mirrors <span className="mono">crates/agent-algo-order/src/main.rs</span> (<span className="mono">fn dispatcher</span> + per-algo runners).</p>
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
  </>);
}
