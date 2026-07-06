"use client";

import { useCallback, useEffect, useState } from "react";
import { RetentionForm, type FormValues } from "./components/RetentionForm";
import { RetentionChart } from "./components/RetentionChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { RetentionState } from "@/lib/retention-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: RetentionState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/audit-retention/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/audit-retention/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const reset = async () => { await fetch("/api/audit-retention/reset", { method: "POST" }); refresh(); };
  const rotate = async () => { await fetch("/api/audit-retention/rotate", { method: "POST" }); refresh(); };
  const verify = async () => { await fetch("/api/audit-retention/verify", { method: "POST" }); refresh(); };
  const tamper = async (num: number) => { await fetch("/api/audit-retention/tamper", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ num }) }); refresh(); };

  const loaded = snap.agent !== null;

  return (<>
    <TopBar active={tab} onChange={setTab} live={loaded} />
    <div className="container">
      {tab === "dashboard" && (<>
        <InfoGrid agent={snap.agent} />
        <div className="two-col">
          <div className="stack">
            <div className="card">
              <h2>Retention setup</h2>
              <RetentionForm disabled={false} onStart={start} />
              {loaded && (<div className="row" style={{ marginTop: 12, gap: 8 }}><button className="ghost" onClick={reset}>Reset</button></div>)}
            </div>
            <div className="card"><h2>Demo tools</h2><DemoTools agent={snap.agent} onRotate={rotate} onVerify={verify} onTamper={tamper} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Slice chain · <span className="demobadge">DEMO</span></h2><RetentionChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-audit-retention</strong> splits a large <span className="mono">agent-trading-history</span> JSONL into per-day (or per-week) slice files. Each slice starts with a signed header that embeds the <span className="mono">prev_slice_hash</span> of the previous slice — the audit chain remains provable across rotations. With <span className="mono">--retention-days N</span>, records older than the window are dropped; the surviving slice headers still chain through.</p>
            <p>Try lowering retention or clicking Tamper on a slice — the next Verify will report the first broken slice.</p>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
