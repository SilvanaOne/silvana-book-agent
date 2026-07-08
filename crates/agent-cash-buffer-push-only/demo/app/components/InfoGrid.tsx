"use client";

import type { CashBufferState } from "@/lib/cashbuffer-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

function ago(ms: number | null | undefined): string {
  if (!ms) return "—";
  const secs = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

type Props = Readonly<{ cashbuffer: CashBufferState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ cashbuffer, walk: _walk }: Props) {
  const c = cashbuffer?.config;
  const balance = cashbuffer?.currentBalance ?? null;
  const target = cashbuffer?.target ?? null;
  const overflow = c && balance !== null ? balance > c.maxCc : false;
  const underflow = c && balance !== null ? balance < c.minCc : false;
  const armed = cashbuffer?.status === "monitoring";
  const stateLabel = cashbuffer === null ? "idle" : armed ? (overflow ? "overflow" : underflow ? "underflow" : "in-band") : "stopped";
  const deviation = target !== null && balance !== null ? balance - target : null;
  const devClass = overflow ? "negative" : underflow ? "negative" : "positive";

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">cash-buffer</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">tick interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CB CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">min cc</span><span className="v positive">{fmt(c.minCc)}</span></div>
            <div className="kv-row"><span className="k">max cc</span><span className="v negative">{fmt(c.maxCc)}</span></div>
            <div className="kv-row"><span className="k">sink party</span><span className="v mono">{c.sinkParty}</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v">{c.checkIntervalSecs}s</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">min cc</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">max cc</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">sink party</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">check interval</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">LIVE STATE</div></div>
        <div className="kv-row"><span className="k">current balance</span><span className="v accent">{fmt(balance)}</span></div>
        <div className="kv-row"><span className="k">state</span><span className={`v ${stateLabel === "overflow" ? "negative" : stateLabel === "underflow" ? "negative" : stateLabel === "in-band" ? "positive" : ""}`}>{stateLabel}</span></div>
        <div className="kv-row"><span className="k">overflow?</span><span className={`v ${overflow ? "negative" : ""}`}>{overflow ? "yes" : "no"}</span></div>
        <div className="kv-row"><span className="k">underflow?</span><span className={`v ${underflow ? "negative" : ""}`}>{underflow ? "yes" : "no"}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TARGET</div></div>
        <div className="kv-row"><span className="k">target = (min+max)/2</span><span className="v accent">{fmt(target)}</span></div>
        <div className="kv-row"><span className="k">deviation from target</span><span className={`v ${deviation !== null && deviation !== 0 ? devClass : ""}`}>{deviation !== null ? (deviation >= 0 ? "+" : "") + fmt(deviation) : "—"}</span></div>
        <div className="kv-row"><span className="k">income / tick</span><span className="v">{c ? fmt(c.incomeRate) : "—"}</span></div>
        <div className="kv-row"><span className="k">last check</span><span className="v">{ago(cashbuffer?.lastCheckAt ?? null)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PUSHES</div></div>
        <div className="kv-row"><span className="k">pushes count</span><span className="v accent">{cashbuffer?.pushesCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">total pushed</span><span className="v">{fmt(cashbuffer?.totalPushed ?? 0)}</span></div>
        <div className="kv-row"><span className="k">last push amount</span><span className="v">{fmt(cashbuffer?.lastPushAmount ?? null)}</span></div>
        <div className="kv-row"><span className="k">last push at</span><span className="v">{ago(cashbuffer?.lastPushAt ?? null)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">WARNINGS</div></div>
        <div className="kv-row"><span className="k">warnings count</span><span className={`v ${(cashbuffer?.warningsCount ?? 0) > 0 ? "negative" : ""}`}>{cashbuffer?.warningsCount ?? 0}</span></div>
        <div className="kv-row"><span className="k">last warning at</span><span className="v">{ago(cashbuffer?.lastWarningAt ?? null)}</span></div>
        <div className="kv-row"><span className="k">reason</span><span className="v faint">balance &lt; min_cc</span></div>
        <div className="kv-row"><span className="k">action</span><span className="v faint">push-only, log</span></div>
      </div>
    </div>
  );
}
