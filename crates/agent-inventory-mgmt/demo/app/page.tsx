"use client";

import { useCallback, useEffect, useState } from "react";
import { InventoryMgmtForm, type FormValues } from "./components/InventoryMgmtForm";
import { InventoryMgmtChart } from "./components/InventoryMgmtChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { InventoryMgmtState } from "@/lib/inventorymgmt-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  inventorymgmt: InventoryMgmtState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ inventorymgmt: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/inventory-mgmt/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      instrument: v.instrument,
      target: v.target,
      tolerance: v.tolerance,
      chunkSize: v.chunkSize,
      checkIntervalSecs: v.checkIntervalSecs,
      priceOffsetPct: v.priceOffsetPct,
      startingBalance: v.startingBalance,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/inventory-mgmt/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/inventory-mgmt/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/inventory-mgmt/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.inventorymgmt?.status === "monitoring";
  const hasIm = snap.inventorymgmt !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid inventorymgmt={snap.inventorymgmt} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Inventory Mgmt setup</h2>
                  <InventoryMgmtForm disabled={running} onStart={start} />
                  {hasIm && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools inventorymgmt={snap.inventorymgmt} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid + balance vs target band · <span className="demobadge">DEMO</span></h2>
                  <InventoryMgmtChart ticks={snap.ticks} inventorymgmt={snap.inventorymgmt} />
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
              <p><strong>agent-inventory-mgmt</strong> — keeps the unlocked balance of a single instrument inside the target band <span className="mono">[target − tolerance, target + tolerance]</span>. On every check interval it inspects the balance and, if it is outside the band, places one <span className="mono">chunk_size</span> order on the configured market to push it back: an OFFER (sell) when balance is above the band, a BID (buy) when it is below. Order price is <span className="mono">mid × (1 + offset/100)</span>.</p>
              <p>The placement engine (<span className="mono">lib/inventorymgmt-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-inventory-mgmt/src/main.rs</span>. Prices are a GBM random walk — no real orders are sent, and fills are simulated whenever the mid crosses a resting order. Only one open rebalance order per side at any time — no stacking.</p>
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
