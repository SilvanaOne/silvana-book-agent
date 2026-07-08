"use client";

import { useCallback, useEffect, useState } from "react";
import { ReplayForm, type FormValues } from "./components/ReplayForm";
import { ReplayChart } from "./components/ReplayChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ReplayState } from "@/lib/replay-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: ReplayState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/audit-replay-back-test/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/audit-replay-back-test/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const reset = async () => { await fetch("/api/audit-replay-back-test/reset", { method: "POST" }); refresh(); };
  const reload = async () => { await fetch("/api/audit-replay-back-test/reload", { method: "POST" }); refresh(); };
  const rerun = async () => { await fetch("/api/audit-replay-back-test/rerun", { method: "POST" }); refresh(); };

  const loaded = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={loaded} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Policy setup</h2>
              <ReplayForm disabled={false} onStart={start} />
              {loaded && (<div className="row" style={{ marginTop: 12, gap: 8 }}><button className="ghost" onClick={reset}>Reset</button></div>)}
            </div>
            <div className="card"><h2>Demo tools</h2><DemoTools agent={snap.agent} onReload={reload} onReRun={rerun} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Verdicts + rule hits · <span className="demobadge">DEMO</span></h2><ReplayChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-audit-replay-back-test</strong> reads a signed trading-history JSONL and replays every event through a TOML rule set (same shape as <span className="mono">pre-trade-check</span> / <span className="mono">compliance-screening</span>). Emits per-event verdicts + a summary — answer &quot;how many trades would have been blocked under this new policy?&quot; before rolling it out live.</p>
            <p>This demo generates a synthetic history in-browser and lets you tighten rules + re-run. The engine (<span className="mono">lib/replay-engine.ts</span>) mirrors <span className="mono">crates/agent-audit-replay-back-test/src/main.rs</span>.</p>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
