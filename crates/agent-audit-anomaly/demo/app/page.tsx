"use client";

import { useCallback, useEffect, useState } from "react";
import { AnomalyForm, type FormValues } from "./components/AnomalyForm";
import { AnomalyChart } from "./components/AnomalyChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { AnomalyState } from "@/lib/anomaly-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: AnomalyState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/audit-anomaly/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/audit-anomaly/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const reset = async () => { await fetch("/api/audit-anomaly/reset", { method: "POST" }); refresh(); };
  const reload = async () => { await fetch("/api/audit-anomaly/reload", { method: "POST" }); refresh(); };
  const rescan = async () => { await fetch("/api/audit-anomaly/rescan", { method: "POST" }); refresh(); };

  const loaded = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={loaded} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Scan setup</h2>
              <AnomalyForm disabled={false} onStart={start} />
              {loaded && (<div className="row" style={{ marginTop: 12, gap: 8 }}><button className="ghost" onClick={reset}>Reset</button></div>)}
            </div>
            <div className="card"><h2>Demo tools</h2><DemoTools agent={snap.agent} onReload={reload} onReScan={rescan} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Anomalies · <span className="demobadge">DEMO</span></h2><AnomalyChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-audit-anomaly</strong> scans a signed trading-history JSONL for statistical anomalies: <span className="mono">stuck_settlement</span> (fill with no settled/failed inside the window), <span className="mono">rapid_cancel</span> (create+cancel within ms — spoofing indicator), <span className="mono">layer_cluster</span> (many same-side orders inside a tight price band), <span className="mono">fill_before_cancel_burst</span> (burst of cancels right after a fill — churn indicator). Distinct from live <span className="mono">agent-market-abuse</span>: works on any historical log, no stream connection.</p>
            <p>This demo generates a synthetic history with optional injected anomalies. Try tightening thresholds and clicking Re-scan on the same log — you&apos;ll see the anomaly count climb.</p>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
