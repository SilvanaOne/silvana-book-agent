"use client";

import { useCallback, useEffect, useState } from "react";
import { TradingHistoryForm, type FormValues } from "./components/TradingHistoryForm";
import { TradingHistoryChart } from "./components/TradingHistoryChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { TradingHistoryState } from "@/lib/tradinghistory-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  tradinghistory: TradingHistoryState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ tradinghistory: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/trading-history/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      orders: v.orders,
      settlements: v.settlements,
      market: v.market,
      eventArrivalPerTick: v.eventArrivalPerTick,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/trading-history/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/trading-history/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/trading-history/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };
  const tamper = async (index: number) => {
    const r = await fetch("/api/trading-history/tamper", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ index }) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };

  const running = snap.tradinghistory?.status === "monitoring";
  const hasTh = snap.tradinghistory !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid tradinghistory={snap.tradinghistory} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Trading History setup</h2>
                  <TradingHistoryForm disabled={running} onStart={start} />
                  {hasTh && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools tradinghistory={snap.tradinghistory} onJump={jump} onWalk={walk} onTamper={tamper} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Signed hash chain · <span className="demobadge">DEMO</span></h2>
                  <TradingHistoryChart tradinghistory={snap.tradinghistory} />
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
              <p><strong>agent-trading-history</strong> — subscribes to a party&apos;s own <span className="mono">order.*</span> + <span className="mono">settlement.*</span> streams and appends every event to a JSONL file as a signed, hash-chained record. Each line carries a <span className="mono">prev_hash</span> pointing at the previous line&apos;s hash and an Ed25519 signature over <span className="mono">(seq, ts, prev_hash, kind, sha256(payload))</span>. Editing any single line breaks the chain from that point forward — that&apos;s exactly what the <em>Tamper record</em> button demonstrates.</p>
              <p>This demo uses a Poisson arrival process to simulate order/settlement events and a lightweight non-cryptographic hash so you can watch the chain form and break in real time. The full agent (see <span className="mono">crates/agent-trading-history/src/main.rs</span>) uses SHA-256 + Ed25519 and can be re-verified offline with <span className="mono">agent-trading-history verify --history-file &lt;path&gt;</span>.</p>
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
