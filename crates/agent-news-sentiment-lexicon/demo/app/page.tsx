"use client";

import { useCallback, useEffect, useState } from "react";
import { NewsForm, type FormValues } from "./components/NewsForm";
import { NewsChart } from "./components/NewsChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { NewsState } from "@/lib/news-engine";
import type { EventEntry } from "@/lib/store";

type Snapshot = { agent: NewsState | null; events: EventEntry[] };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ agent: null, events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try { const r = await fetch("/api/news-sentiment-lexicon/state", { cache: "no-store" }); const j = (await r.json()) as Snapshot; setSnap(j); } catch { /* ignore */ }
  }, []);
  useEffect(() => { refresh(); const id = setInterval(refresh, 500); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const r = await fetch("/api/news-sentiment-lexicon/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(v) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/news-sentiment-lexicon/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/news-sentiment-lexicon/reset", { method: "POST" }); refresh(); };
  const ingest = async (text: string) => { await fetch("/api/news-sentiment-lexicon/ingest", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ text }) }); refresh(); };

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
              <h2>Feed setup</h2>
              <NewsForm disabled={running} onStart={start} />
              {hasAgent && (
                <div className="row" style={{ marginTop: 12, gap: 8 }}>
                  {running && <button className="danger" onClick={stop}>Stop</button>}
                  <button className="ghost" onClick={reset}>Reset</button>
                </div>
              )}
            </div>
            <div className="card"><h2>Demo tools</h2><DemoTools agent={snap.agent} onIngest={ingest} /></div>
          </div>
          <div className="stack">
            <div className="card"><h2>Headlines · <span className="demobadge">DEMO</span></h2><NewsChart agent={snap.agent} /></div>
            <div className="card"><h2>Recent events</h2><EventsLog events={snap.events.slice(-10)} /></div>
          </div>
        </div>
      </>)}
      {tab === "events" && (<div className="card"><h2>Events log</h2><EventsLog events={snap.events} /></div>)}
      {tab === "docs" && (
        <div className="card">
          <h2>About this demo</h2>
          <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 13.5 }}>
            <p><strong>agent-news-sentiment-lexicon</strong> streams news headlines, scores each on a lexicon in <span className="mono">[-1..+1]</span>, extracts market mentions via a <span className="mono">market → aliases</span> map, and emits a signal to <span className="mono">agent-signal-bot</span> when <span className="mono">|score| &gt; threshold</span> AND text mentions a configured market. This demo runs a deterministic scorer identical to the Rust binary — swap in Vader/RoBERTa in prod behind the same output schema.</p>
          </div>
        </div>
      )}
    </div>
    <Footer />
  </>);
}
