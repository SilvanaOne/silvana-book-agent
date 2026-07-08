"use client";

import { useCallback, useEffect, useState } from "react";
import { ExplainForm, type FormValues } from "./components/ExplainForm";
import { ExplainChart } from "./components/ExplainChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ExplainState } from "@/lib/explain-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: ExplainState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/trade-explain-rationale/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/trade-explain-rationale/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const reset = async () => { await fetch("/api/trade-explain-rationale/reset", { method: "POST" }); refresh(); };
  const rerun = async () => { await fetch("/api/trade-explain-rationale/rerun", { method: "POST" }); refresh(); };

  const loaded = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={loaded} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Setup</h2>
              <ExplainForm disabled={false} onStart={start} />
              {loaded && (<div className="row" style={{ marginTop: 12, gap: 8 }}><button className="ghost" onClick={reset}>Reset</button></div>)}
            </div>
            <div className="card"><h2>Demo tools</h2><DemoTools agent={snap.agent} onReRun={rerun} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Rationale + counterfactual · <span className="demobadge">DEMO</span></h2><ExplainChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-trade-explain-rationale</strong> reads a trading-history JSONL and generates, for every <span className="mono">order.created</span> or <span className="mono">settlement.settled</span>, a short rationale plus a counterfactual (&quot;what likely would have happened if we hadn&apos;t?&quot;). Output is a signed JSONL — auditors can attach it as decision-support alongside the trades themselves.</p>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
