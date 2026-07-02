"use client";

import { useCallback, useEffect, useState } from "react";
import { OracleForm, type FormValues } from "./components/OracleForm";
import { OracleChart } from "./components/OracleChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { OracleState } from "@/lib/oracle-engine";
import type { EventEntry, MarketTick } from "@/lib/store";

type Snapshot = {
  oracle: OracleState | null;
  ticks: MarketTick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ oracle: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.005 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/oracle/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const markets = v.marketsCsv.split(",").map((m) => m.trim()).filter((m) => m.length > 0);
    const body = {
      markets,
      source: v.source,
      pollSecs: Number(v.pollSecs),
      startingPrice: Number(v.startingPrice),
    };
    const r = await fetch("/api/oracle/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/oracle/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/oracle/reset", { method: "POST" }); refresh(); };

  const running = snap.oracle?.status === "publishing";
  const hasOracle = snap.oracle !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid oracle={snap.oracle} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Oracle setup</h2>
                  <OracleForm disabled={running} onStart={start} />
                  {hasOracle && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Info</h2>
                  <DemoTools oracle={snap.oracle} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Price feed · normalized · publish events · <span className="demobadge">DEMO</span></h2>
                  <OracleChart ticks={snap.ticks} oracle={snap.oracle} />
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
              <p><strong>agent-oracle</strong> — a scheduled price publisher. Every <span className="mono">pollSecs</span> seconds it polls the configured Silvana pricing feed (via <span className="mono">GetPrice</span>) for every market in <span className="mono">--markets</span> and emits one structured record per market per poll to sinks (stdout, JSONL, webhook). <span className="mono">--source</span> optionally pins upstream to <span className="mono">binance_spot</span>, <span className="mono">bybit</span>, or <span className="mono">coingecko</span>.</p>
              <p>In this demo prices are simulated per market with independent GBM random walks; the chart normalizes each market to its starting price (1.0×) so multiple scales share one axis. Orange dots mark publish events — one per market at every <span className="mono">pollSecs</span> boundary. The engine (<span className="mono">lib/oracle-engine.ts</span>) mirrors the Rust polling loop in <span className="mono">crates/agent-oracle/src/main.rs</span>.</p>
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
