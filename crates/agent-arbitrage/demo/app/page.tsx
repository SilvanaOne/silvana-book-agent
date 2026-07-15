"use client";

import { useCallback, useEffect, useState } from "react";
import { ArbitrageForm, type FormValues } from "./components/ArbitrageForm";
import { SpreadChart } from "./components/SpreadChart";
import { RecentSpreads } from "./components/RecentSpreads";
import { Profitability } from "./components/Profitability";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { ArbitrageState } from "@/lib/arbitrage-engine";
import type { WalkParams } from "@/lib/spread-simulator";
import type { EventEntry } from "@/lib/store";

type Snapshot = {
  arbitrage: ArbitrageState | null;
  events: EventEntry[];
  walk: WalkParams;
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ arbitrage: null, events: [], walk: { driftPerTick: 0, volPerTick: 6 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/arbitrage/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch {
      /* ignore */
    }
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 1000);
    return () => clearInterval(id);
  }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      focusPair: v.focusPair,
      minSpreadBps: Number(v.minSpreadBps),
      tradeSizeUsd: Number(v.tradeSizeUsd),
      scanIntervalSecs: Number(v.scanIntervalSecs),
    };
    const r = await fetch("/api/arbitrage/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) {
      const j = (await r.json().catch(() => ({}))) as { error?: string };
      throw new Error(j.error ?? `HTTP ${r.status}`);
    }
    refresh();
  };
  const stop = async () => { await fetch("/api/arbitrage/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/arbitrage/reset", { method: "POST" }); refresh(); };
  const nudge = async (to: number) => { await fetch("/api/spread/nudge", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/spread/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.arbitrage?.status === "scanning";
  const hasRun = snap.arbitrage !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid arbitrage={snap.arbitrage} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Scanner setup</h2>
                  <ArbitrageForm disabled={running} onStart={start} />
                  {hasRun && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools arbitrage={snap.arbitrage} onNudge={nudge} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Cross-venue spread · <span className="demobadge">DEMO</span></h2>
                  <SpreadChart arbitrage={snap.arbitrage} />
                </div>
                <div className="card">
                  <h2>Recent spreads · <span className="demobadge">DEMO</span></h2>
                  <RecentSpreads rows={snap.arbitrage?.recent ?? []} />
                </div>
              </div>
            </div>

            <div className="card" style={{ marginTop: 14 }}>
              <h2>Profitability summary · <span className="demobadge">DEMO</span></h2>
              <Profitability arbitrage={snap.arbitrage} />
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
              <p><strong>agent-arbitrage</strong> — a cross-venue spread scanner. It watches the same trading pair across several Canton venues (Silvana, Cantex, OneSwap, Temple) plus a couple of CEXes, and whenever the best buy price on one venue and the best sell price on another diverge, that gap is a spread opportunity measured in basis points. When the spread clears the configured <span className="mono">act threshold</span> the agent would route a buy-low / sell-high pair of orders to capture it.</p>
              <p>Everything here is a self-contained, in-memory simulation. Per-pair spreads follow a mean-reverting random walk around hand-tuned anchors (see <span className="mono">lib/demo-data.ts</span>), each scan cycle emits believable spread rows, and the profitability figures accumulate live on top of the synthetic seed. No real venues are queried and <strong>no real orders are sent</strong> — the <span className="mono">DEMO</span> badges mark every synthetic panel.</p>
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
