"use client";

import { useCallback, useEffect, useState } from "react";
import { CopyForm, type FormValues } from "./components/CopyForm";
import { CopyChart } from "./components/CopyChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { CopyState } from "@/lib/copy-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: CopyState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/copy-trading-mirror/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/copy-trading-mirror/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/copy-trading-mirror/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/copy-trading-mirror/reset", { method: "POST" }); refresh(); };

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
              <h2>Mirror setup</h2>
              <CopyForm disabled={running} onStart={start} />
              {hasAgent && (
                <div className="row" style={{ marginTop: 12, gap: 8 }}>
                  {running && <button className="danger" onClick={stop}>Stop</button>}
                  <button className="ghost" onClick={reset}>Reset</button>
                </div>
              )}
            </div>
            <div className="card"><h2>Demo tools</h2><DemoTools agent={snap.agent} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Leader vs follower · <span className="demobadge">DEMO</span></h2><CopyChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-copy-trading-mirror</strong> subscribes to a leader party&apos;s <span className="mono">SubscribeOrders</span> stream and mirrors every <span className="mono">order.created</span> onto this party&apos;s book at a configurable <span className="mono">scale</span>. Optional filters gate which markets are mirrored (<span className="mono">--markets</span>) and cap how large a single mirror trade can be (<span className="mono">--max-mirror-notional</span>).</p>
            <p>This demo simulates the leader by generating synthetic orders in-browser. The follower&apos;s net position climbs alongside the leader&apos;s, scaled down. Rejected orders (market not on whitelist, notional cap exceeded) are counted per rule. The engine (<span className="mono">lib/copy-engine.ts</span>) mirrors <span className="mono">crates/agent-copy-trading-mirror/src/main.rs</span>.</p>
            <ul style={{ paddingLeft: 18 }}>
              <li><a className="mono" style={{ color: "var(--accent)" }} href="https://github.com/SilvanaOne/silvana-book-agent" target="_blank" rel="noopener">github: silvana-book-agent</a></li>
              <li><a className="mono" style={{ color: "var(--accent)" }} href="https://docs.silvana.one" target="_blank" rel="noopener">docs.silvana.one</a></li>
            </ul>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
