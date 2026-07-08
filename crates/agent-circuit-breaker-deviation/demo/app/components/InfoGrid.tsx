"use client";

import type { CircuitBreakerState } from "@/lib/circuitbreaker-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function pct(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const s = n >= 0 ? "+" : "";
  return `${s}${n.toFixed(3)}%`;
}

function whenAgo(t: number | null | undefined, now: number): string {
  if (t === null || t === undefined) return "—";
  const dSec = Math.max(0, Math.floor((now - t) / 1000));
  if (dSec < 60) return `${dSec}s ago`;
  const m = Math.floor(dSec / 60);
  const s = dSec % 60;
  return `${m}m ${s}s ago`;
}

type Props = Readonly<{ circuitbreaker: CircuitBreakerState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ circuitbreaker, walk }: Props) {
  const c = circuitbreaker?.config;
  const now = Date.now();
  const samples = circuitbreaker?.sampleWindow.length ?? 0;
  const warmupOk = samples > 3;
  const stateLabel = circuitbreaker === null
    ? "idle"
    : circuitbreaker.status === "paused"
      ? "PAUSED"
      : warmupOk ? "armed" : "warmup";
  const stateAccent = circuitbreaker?.status === "paused" ? "negative" : "";
  const secondsToResume = circuitbreaker?.status === "paused" && circuitbreaker.pauseUntil
    ? Math.max(0, Math.ceil((circuitbreaker.pauseUntil - now) / 1000))
    : null;

  const breachedNow = c && circuitbreaker && warmupOk
    ? Math.abs(circuitbreaker.lastDeviationPct) > c.maxDeviationPct
    : false;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">circuit-breaker</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CB CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">max deviation</span><span className="v accent">±{c.maxDeviationPct}%</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v">{c.windowSecs}s</span></div>
            <div className="kv-row"><span className="k">pause</span><span className="v">{c.pauseSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max deviation</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">window</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">pause</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(circuitbreaker?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">baseline</span><span className="v accent">{fmt(circuitbreaker?.baseline ?? null)}</span></div>
        <div className="kv-row"><span className="k">deviation</span><span className={`v ${breachedNow ? "negative" : ""}`}>{pct(circuitbreaker?.lastDeviationPct)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className={`v ${stateAccent}`}>{stateLabel}{secondsToResume !== null ? ` · resume in ${secondsToResume}s` : ""}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">WINDOW</div></div>
        <div className="kv-row"><span className="k">samples in window</span><span className="v">{samples}</span></div>
        <div className="kv-row"><span className="k">window secs</span><span className="v">{c ? `${c.windowSecs}s` : "—"}</span></div>
        <div className="kv-row"><span className="k">warmup (&gt;3)</span><span className="v">{warmupOk ? "ok" : "not yet"}</span></div>
        <div className="kv-row"><span className="k">upper / lower</span><span className="v">{circuitbreaker?.baseline && c ? `${fmt(circuitbreaker.baseline * (1 + c.maxDeviationPct / 100))} / ${fmt(circuitbreaker.baseline * (1 - c.maxDeviationPct / 100))}` : "—"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BREACHES</div></div>
        <div className="kv-row"><span className="k">breaches</span><span className={`v ${circuitbreaker && circuitbreaker.breaches > 0 ? "negative" : ""}`}>{circuitbreaker?.breaches ?? 0}</span></div>
        <div className="kv-row"><span className="k">last breach</span><span className="v">{whenAgo(circuitbreaker?.lastBreachAt ?? null, now)}</span></div>
        <div className="kv-row"><span className="k">open orders (sim)</span><span className="v">{circuitbreaker?.openOrdersMock ?? 0}</span></div>
        <div className="kv-row"><span className="k">cancellations lifetime</span><span className="v">{circuitbreaker?.cancellationsCount ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
        <div className="kv-row"><span className="k">jump tools</span><span className="v">±0.5% / ±2% / ±5%</span></div>
      </div>
    </div>
  );
}
