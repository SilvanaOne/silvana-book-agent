"use client";

import { useCallback, useEffect, useState } from "react";
import { PreTradeCheckForm, ManualCheck, type FormValues } from "./components/PreTradeCheckForm";
import { PreTradeCheckChart } from "./components/PreTradeCheckChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { PreTradeCheckState } from "@/lib/pretradecheck-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  pretradecheck: PreTradeCheckState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ pretradecheck: null, ticks: [], events: [], walk: { driftPerTick: 0, volPerTick: 0.008 } });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/pre-trade-check/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { refresh(); const id = setInterval(refresh, 1000); return () => clearInterval(id); }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      rulesJson: v.rulesJson,
      orderArrivalPerTick: Number(v.orderArrivalPerTick),
      startingPrice: Number(v.startingPrice),
    };
    const r = await fetch("/api/pre-trade-check/start", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    if (!r.ok) { const j = (await r.json().catch(() => ({}))) as { error?: string }; throw new Error(j.error ?? `HTTP ${r.status}`); }
    refresh();
  };
  const stop = async () => { await fetch("/api/pre-trade-check/stop", { method: "POST" }); refresh(); };
  const reset = async () => { await fetch("/api/pre-trade-check/reset", { method: "POST" }); refresh(); };
  const jump = async (to: number) => { await fetch("/api/price/jump", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ to }) }); };
  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => { await fetch("/api/price/walk", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }); };

  const running = snap.pretradecheck?.status === "monitoring";
  const hasPtc = snap.pretradecheck !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid pretradecheck={snap.pretradecheck} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Pre-Trade Check setup</h2>
                  <PreTradeCheckForm disabled={running} onStart={start} />
                  {hasPtc && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop</button>}
                      <button className="ghost" onClick={reset}>Reset</button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Manual check</h2>
                  <p className="muted" style={{ fontSize: 12, marginTop: -6, marginBottom: 10 }}>
                    Validate a single order against the live rules (or the built-in defaults if the engine isn&apos;t running).
                  </p>
                  <ManualCheck />
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools pretradecheck={snap.pretradecheck} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>Decisions · failed rules · price×qty · <span className="demobadge">DEMO</span></h2>
                  <PreTradeCheckChart pretradecheck={snap.pretradecheck} />
                </div>
                <div className="card">
                  <h2>Recent checks</h2>
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
              <p><strong>agent-pre-trade-check</strong> is a pure offline rule engine: no network, no JWT, no ledger writes. It validates each prospective order against a small ruleset — <span className="mono">max_notional_per_order</span>, <span className="mono">max_quantity_per_order</span>, <span className="mono">min_price</span>, <span className="mono">max_price</span>, <span className="mono">blocked_markets</span>, <span className="mono">allowed_markets</span>, <span className="mono">allowed_sides</span> — and returns accept or reject with the list of failed rules. Wire it into CI or as a pre-flight gate before <span className="mono">agent-spot-dca</span>, <span className="mono">orderbook-cloud-agent</span>, etc.</p>
              <p>This demo synthesises a stream of random orders (Poisson-arrival λ per tick) drawn from CC-USDC, BTC-USD and a deliberately-blocked <span className="mono">XXX-YYY</span> market with occasional out-of-band prices, then feeds each through the engine. The engine lives in <span className="mono">lib/pretradecheck-engine.ts</span> and mirrors the Rust rules in <span className="mono">crates/agent-pre-trade-check/src/main.rs</span>.</p>
              <p>You can also send a single order through the current rules via <em>Manual check</em>, which hits <span className="mono">POST /api/pre-trade-check/check-one</span> — the same shape the CLI uses.</p>
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
