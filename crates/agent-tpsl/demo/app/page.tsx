"use client";

import { useCallback, useEffect, useState } from "react";
import { PositionForm, type FormValues } from "./components/PositionForm";
import { PriceChart } from "./components/PriceChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { StatusStrip } from "./components/StatusStrip";
import { MetricStrip } from "./components/MetricStrip";
import { Footer } from "./components/Footer";
import type { PositionState } from "@/lib/tpsl-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  position: PositionState | null;
  ticks: Tick[];
  events: EventEntry[];
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({ position: null, ticks: [], events: [] });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/tpsl/state", { cache: "no-store" });
      const j = (await r.json()) as Snapshot;
      setSnap(j);
    } catch { /* ignore transient */ }
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 1000);
    return () => clearInterval(id);
  }, [refresh]);

  const start = async (v: FormValues) => {
    const body = {
      market: v.market,
      side: v.side,
      quantity: v.quantity,
      entryPrice: v.entryPrice,
      tp: v.tp === "" ? null : v.tp,
      sl: v.sl === "" ? null : v.sl,
      trailingPct: v.trailingPct === "" ? null : v.trailingPct,
    };
    const r = await fetch("/api/tpsl/start", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!r.ok) {
      const j = (await r.json().catch(() => ({}))) as { error?: string };
      throw new Error(j.error ?? `HTTP ${r.status}`);
    }
    refresh();
  };

  const stop = async () => {
    await fetch("/api/tpsl/stop", { method: "POST" });
    refresh();
  };

  const reset = async () => {
    await fetch("/api/tpsl/reset", { method: "POST" });
    refresh();
  };

  const jump = async (to: number) => {
    await fetch("/api/price/jump", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ to }),
    });
  };

  const walk = async (patch: { driftPerTick?: number; volPerTick?: number }) => {
    await fetch("/api/price/walk", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    });
  };

  const running = snap.position?.status === "monitoring";
  const triggered = snap.position?.status === "triggered";
  const hasPosition = snap.position !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} />
      <StatusStrip position={snap.position} tickCount={snap.ticks.length} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <MetricStrip position={snap.position} />

            <div style={{ display: "grid", gridTemplateColumns: "minmax(320px, 380px) 1fr", gap: 16 }}>
              <div className="stack">
                <div className="card">
                  <h2>Position setup</h2>
                  <PositionForm disabled={running || triggered} onStart={start} />
                  {(running || triggered) && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && <button className="danger" onClick={stop}>Stop monitor</button>}
                      {hasPosition && <button className="ghost" onClick={reset}>Reset</button>}
                    </div>
                  )}
                </div>

                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools position={snap.position} onJump={jump} onWalk={walk} />
                </div>
              </div>

              <div className="stack">
                <div className="card">
                  <h2>Price chart</h2>
                  <PriceChart ticks={snap.ticks} position={snap.position} />
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
            <h2>Full events log</h2>
            <EventsLog events={snap.events} />
          </div>
        )}

        {tab === "docs" && (
          <div className="card">
            <h2>About this demo</h2>
            <div style={{ maxWidth: 720, lineHeight: 1.7, fontSize: 14 }}>
              <p>
                <strong>agent-tpsl</strong> monitors a live orderbook price on Canton and automatically
                fires an exit order when Take-Profit or Stop-Loss levels are hit. Trailing stops ratchet
                with peak (long) or trough (short) so profit is locked in as price moves in the
                position's favour.
              </p>
              <p>
                This is a fully-simulated environment. A geometric Brownian random walk drives the
                synthetic price series; the trigger engine (<span className="mono">lib/tpsl-engine.ts</span>)
                is a byte-for-byte port of the Rust rules in{" "}
                <span className="mono">crates/agent-tpsl/src/main.rs</span>. When a level fires the UI
                shows the exit action the real agent would submit — <em>no orders are sent to any orderbook</em>.
              </p>
              <p>
                Use the <em>Demo tools</em> to nudge or teleport the price to specific levels — perfect
                for showing how trailing stops ratchet, or how a stop hits before a take-profit on the
                same setup.
              </p>
              <ul style={{ paddingLeft: 20 }}>
                <li>
                  <span className="mono">GitHub</span> — source, agent code + this demo:{" "}
                  <a className="mono" href="https://github.com/SilvanaOne/silvana-book-agent" target="_blank" rel="noopener">
                    silvana-book-agent
                  </a>
                </li>
                <li>
                  <span className="mono">Docs</span> — full agent catalog and quickstarts at{" "}
                  <a className="mono" href="https://docs.silvana.one" target="_blank" rel="noopener">
                    docs.silvana.one
                  </a>
                </li>
                <li>
                  <span className="mono">Ledger</span> — Silvana runs on{" "}
                  <a className="mono" href="https://canton.network" target="_blank" rel="noopener">
                    Canton Network
                  </a>
                </li>
              </ul>
            </div>
          </div>
        )}
      </div>

      <Footer />
    </>
  );
}
