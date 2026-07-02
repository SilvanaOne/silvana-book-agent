"use client";

import { useCallback, useEffect, useState } from "react";
import { SignatureForm, ManualSignPanel, type FormValues } from "./components/SignatureForm";
import { SignatureChart } from "./components/SignatureChart";
import { EventsLog } from "./components/EventsLog";
import { DemoTools } from "./components/DemoTools";
import { TopBar } from "./components/TopBar";
import { InfoGrid } from "./components/InfoGrid";
import { Footer } from "./components/Footer";
import type { SignatureState } from "@/lib/signature-engine";
import type { EventEntry, Tick } from "@/lib/store";

type Snapshot = {
  signature: SignatureState | null;
  ticks: Tick[];
  events: EventEntry[];
  walk: { driftPerTick: number; volPerTick: number };
};

export default function Home() {
  const [snap, setSnap] = useState<Snapshot>({
    signature: null,
    ticks: [],
    events: [],
    walk: { driftPerTick: 0, volPerTick: 0.008 },
  });
  const [tab, setTab] = useState<"dashboard" | "events" | "docs">("dashboard");

  const refresh = useCallback(async () => {
    try {
      const r = await fetch("/api/signature/state", { cache: "no-store" });
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
      autoDemoIntervalSecs: v.autoDemoIntervalSecs,
      tamperRate: v.tamperRate,
      startingPrice: v.startingPrice,
    };
    const r = await fetch("/api/signature/start", {
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
    await fetch("/api/signature/stop", { method: "POST" });
    refresh();
  };
  const reset = async () => {
    await fetch("/api/signature/reset", { method: "POST" });
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

  const doSign = async (message: string) => {
    const r = await fetch("/api/signature/sign", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ message }),
    });
    const j = (await r.json()) as { signature?: string; publicKey?: string; error?: string };
    if (!r.ok || j.error) return { error: j.error ?? `HTTP ${r.status}` };
    refresh();
    return { signature: j.signature ?? "", publicKey: j.publicKey ?? "" };
  };
  const doVerify = async (message: string, signature: string) => {
    const r = await fetch("/api/signature/verify", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ message, signature }),
    });
    const j = (await r.json()) as { ok?: boolean; error?: string };
    if (!r.ok || j.error) return { error: j.error ?? `HTTP ${r.status}` };
    refresh();
    return { ok: j.ok === true };
  };
  const doRotate = async () => {
    const r = await fetch("/api/signature/rotate-key", { method: "POST" });
    const j = (await r.json()) as { publicKey?: string; error?: string };
    if (!r.ok || j.error) return { error: j.error ?? `HTTP ${r.status}` };
    refresh();
    return { publicKey: j.publicKey ?? "" };
  };

  const running = snap.signature?.status === "monitoring";
  const hasAgent = snap.signature !== null;

  return (
    <>
      <TopBar active={tab} onChange={setTab} live={running} />

      <div className="container">
        {tab === "dashboard" && (
          <>
            <InfoGrid signature={snap.signature} walk={snap.walk} />

            <div className="two-col">
              <div className="stack">
                <div className="card">
                  <h2>Signature setup</h2>
                  <SignatureForm disabled={running} onStart={start} />
                  {hasAgent && (
                    <div className="row" style={{ marginTop: 12, gap: 8 }}>
                      {running && (
                        <button className="danger" onClick={stop}>
                          Stop
                        </button>
                      )}
                      <button className="ghost" onClick={reset}>
                        Reset
                      </button>
                    </div>
                  )}
                </div>
                <div className="card">
                  <h2>Manual sign / verify</h2>
                  <ManualSignPanel disabled={!hasAgent} onSign={doSign} onVerify={doVerify} onRotate={doRotate} />
                </div>
                <div className="card">
                  <h2>Demo tools</h2>
                  <DemoTools signature={snap.signature} onJump={jump} onWalk={walk} />
                </div>
              </div>
              <div className="stack">
                <div className="card">
                  <h2>
                    Operations log · <span className="demobadge">DEMO</span>
                  </h2>
                  <SignatureChart signature={snap.signature} />
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
              <p>
                <strong>agent-signature</strong> — CLI wrapper around Ed25519 signing used off-chain. The real
                binary reads a Base58-encoded private key from{" "}
                <span className="mono">PARTY_AGENT_PRIVATE_KEY</span> and exposes four operations:{" "}
                <span className="mono">public-key</span>, <span className="mono">sign-raw</span>,{" "}
                <span className="mono">sign-canonical</span> (the <span className="mono">ed25519-sha256-v1</span>{" "}
                scheme used by the Ledger gRPC service), and <span className="mono">verify-raw</span> /{" "}
                <span className="mono">verify-canonical</span>. The private key never leaves the process.
              </p>
              <p>
                This demo runs a <strong>mocked</strong> Ed25519 scheme so it needs no native crypto binding. Every{" "}
                <span className="mono">auto-demo interval</span> seconds it auto-generates a synthetic{" "}
                <span className="mono">&quot;trade #N at price X&quot;</span> message, signs it, then verifies it —
                with a <span className="mono">tamper-rate</span> probability of flipping one byte of the signature
                first (which is expected to fail verification). Use the manual panel to sign your own message, verify
                a pasted signature, or rotate the keypair (which invalidates every previous signature).
              </p>
              <ul style={{ paddingLeft: 18 }}>
                <li>
                  <a
                    className="mono"
                    style={{ color: "var(--accent)" }}
                    href="https://github.com/SilvanaOne/silvana-book-agent"
                    target="_blank"
                    rel="noopener"
                  >
                    github: silvana-book-agent
                  </a>
                </li>
                <li>
                  <a
                    className="mono"
                    style={{ color: "var(--accent)" }}
                    href="https://docs.silvana.one"
                    target="_blank"
                    rel="noopener"
                  >
                    docs.silvana.one
                  </a>
                </li>
                <li>
                  <a
                    className="mono"
                    style={{ color: "var(--accent)" }}
                    href="https://canton.network"
                    target="_blank"
                    rel="noopener"
                  >
                    canton.network
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
