"use client";

import { useCallback, useEffect, useState } from "react";
import { ClassifierForm, type FormValues } from "./components/ClassifierForm";
import { ClassifierChart } from "./components/ClassifierChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ClassifierState } from "@/lib/classifier-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: ClassifierState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/anomaly-classifier/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/anomaly-classifier/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/anomaly-classifier/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/anomaly-classifier/reset", { method: "POST" }); refresh(); };

  const running = snap.agent?.status === "running";
  const hasAgent = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={running} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Classifier setup</h2>
              <ClassifierForm disabled={running} onStart={start} />
              {hasAgent && (
                <div className="row" style={{ marginTop: 12, gap: 8 }}>
                  {running && <button className="danger" onClick={stop}>Stop</button>}
                  <button className="ghost" onClick={reset}>Reset</button>
                </div>
              )}
            </div>
            <div className="card"><h2>Rules</h2><DemoTools agent={snap.agent} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Live classifications · <span className="demobadge">DEMO</span></h2><ClassifierChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-anomaly-classifier</strong> consumes the anomaly stream from <span className="mono">agent-audit-anomaly</span> and clusters each record into an abuse category (<span className="mono">spoofing / wash_trading / stuck_settlement / normal_volatility</span>) via co-occurrence over a sliding window. Deterministic rule-based classifier here — drop in an ML model behind the same interface in prod.</p>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
