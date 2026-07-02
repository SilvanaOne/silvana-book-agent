"use client";

import { useCallback, useEffect, useState } from "react";
import { SignalBotForm, type FormValues } from "./components/SignalBotForm";
import { SignalBotChart } from "./components/SignalBotChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { SignalBotState } from "@/lib/signalbot-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  signalbot: SignalBotState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

type InjectForm = { market: string; side: "buy" | "sell"; quantity: string; price: string; ref: string };

const INJECT_DEFAULT: InjectForm = { market: "CC-USDC", side: "buy", quantity: "1", price: "0.15", ref: "" };

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ signalbot: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");
  const [inject, setInject] = useState<InjectForm>(INJECT_DEFAULT);
  const [injectErr, setInjectErr] = useState<string | null>(null);
  const [injectBusy, setInjectBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/signal-bot/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      signalsFilePath: v.signalsFilePath,
      fromEnd: v.fromEnd,
      dryRun: v.dryRun,
      signalArrivalPerTick: v.signalArrivalPerTick,
      rejectionRate: v.rejectionRate,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/signal-bot/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/signal-bot/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/signal-bot/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const submitInject = async (e: React.FormEvent) => {
    e.preventDefault();
    setInjectErr(null);
    setInjectBusy(true);
    try {
      const body = {
        market: inject.market,
        side: inject.side,
        quantity: Number(inject.quantity),
        price: Number(inject.price),
        ref: inject.ref.trim() || undefined,
      };
      const r = await fetch("/api/signal-bot/inject", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
      if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
      refresh();
    } catch (ex) {
      setInjectErr((ex as Error).message);
    } finally {
      setInjectBusy(false);
    }
  };

  const running = snap.signalbot?.status === "monitoring";
  const hasBot = snap.signalbot !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid signalbot={snap.signalbot} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Signal Bot setup</h2>
                  <SignalBotForm disabled={running} onStart={start} />
                  {hasBot && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Inject signal</h2>
                  {!running ? (
                    <div className="muted">Start Signal Bot to inject signals.</div>
                  ) : (
                    <form onSubmit={submitInject} className="stack">
                      <div className="grid-2">
                        <div>
                          <label>Market</label>
                          <input value={inject.market} onChange={(e) => setInject((p) => ({ ...p, market: e.target.value }))} disabled={injectBusy} />
                        </div>
                        <div>
                          <label>Side</label>
                          <select value={inject.side} onChange={(e) => setInject((p) => ({ ...p, side: e.target.value as "buy" | "sell" }))} disabled={injectBusy}>
                            <option value="buy">buy</option>
                            <option value="sell">sell</option>
                          </select>
                        </div>
                      </div>
                      <div className="grid-2">
                        <div>
                          <label>Quantity</label>
                          <input type="number" step="any" value={inject.quantity} onChange={(e) => setInject((p) => ({ ...p, quantity: e.target.value }))} disabled={injectBusy} />
                        </div>
                        <div>
                          <label>Price</label>
                          <input type="number" step="any" value={inject.price} onChange={(e) => setInject((p) => ({ ...p, price: e.target.value }))} disabled={injectBusy} />
                        </div>
                      </div>
                      <div>
                        <label>Ref (optional)</label>
                        <input value={inject.ref} onChange={(e) => setInject((p) => ({ ...p, ref: e.target.value }))} disabled={injectBusy} placeholder="my-strategy-1" />
                      </div>
                      {injectErr && <div className="negative mono" style={{ fontSize: 13 }}>{injectErr}</div>}
                      <button type="submit" className="primary" disabled={injectBusy}>{injectBusy ? "Injecting…" : "Inject signal"}</button>
                    </form>
                  )}
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools signalbot={snap.signalbot} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Mid · signals · orders · cursor · <span className="demobadge">DEMO</span></h2>
                  <SignalBotChart ticks={snap.ticks} signalbot={snap.signalbot} />
                </div>
                <div className="card">
                  <h2>Recent events</h2>
                  <EventsLog events={snap.events.slice(-10)} />
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
              <p><strong>agent-signal-bot</strong> — tails a JSONL file of trading signals and submits one limit order per line. Each JSONL line is <span className="mono">{`{"market":"CC-USDC","side":"buy","quantity":"0.5","price":"1.02","ref":"..."}`}</span>. The bot persists its byte offset in a sibling <span className="mono">&lt;signals&gt;.cursor</span> file so it survives restarts without replaying history.</p>
              <p>The demo simulates the tail loop: a Poisson-ish arrival synthesizes signals per tick (rate <span className="mono">λ</span>), and you can also inject a signal by hand from the panel on the left. Each ingested signal becomes a <span className="mono">submitted</span> order (or <span className="mono">rejected</span> if the price is outside a sanity band or a random gateway rejection fires). Fills happen when the mid crosses a resting order — BID when mid ≤ price, OFFER when mid ≥ price. In <span className="mono">dry-run</span> submits are logged but never fill.</p>
              <p>The placement engine (<span className="mono">lib/signalbot-engine.ts</span>) mirrors the Rust rules in <span className="mono">crates/agent-signal-bot/src/main.rs</span>. Prices are a GBM random walk — no real orders sent.</p>
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
