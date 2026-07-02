"use client";

import type { IcebergExecutionState } from "@/lib/icebergexecution-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 8;
  return n.toFixed(digits);
}

type Props = Readonly<{ icebergexecution: IcebergExecutionState | null; walk: { driftPerTick: number; volPerTick: number } }>;

export function InfoGrid({ icebergexecution, walk }: Props) {
  const c = icebergexecution?.config;
  const stateLabel = icebergexecution === null
    ? "idle"
    : icebergexecution.status === "monitoring"
      ? "working"
      : icebergexecution.status === "completed"
        ? "completed"
        : "stopped";

  const now = Date.now();
  const startedAt = icebergexecution?.startedAt ?? null;
  const completedAt = icebergexecution?.completedAt ?? null;
  const elapsedMs = startedAt === null ? 0 : (completedAt ?? now) - startedAt;
  const elapsedSecs = Math.floor(elapsedMs / 1000);

  const cur = icebergexecution?.currentChunkOrder ?? null;
  const remaining = icebergexecution ? Math.max(0, icebergexecution.config.total - icebergexecution.totalFilled) : 0;
  const totalTarget = icebergexecution?.config.total ?? 0;
  const totalFilled = icebergexecution?.totalFilled ?? 0;
  const pctFilled = totalTarget > 0 ? (totalFilled / totalTarget) * 100 : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">iceberg-execution</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">persist</span><span className="v">off</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">ICE CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">side</span><span className={`v ${c.side === "buy" ? "positive" : "negative"}`}>{c.side}</span></div>
            <div className="kv-row"><span className="k">total</span><span className="v">{c.total}</span></div>
            <div className="kv-row"><span className="k">visible / chunk</span><span className="v">{c.visible}</span></div>
            <div className="kv-row"><span className="k">chunk price</span><span className="v accent">{fmt(c.price)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">total</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">visible / chunk</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">chunk price</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PROGRESS</div></div>
        <div className="kv-row"><span className="k">total filled</span><span className="v accent">{fmt(totalFilled)}</span></div>
        <div className="kv-row"><span className="k">remaining</span><span className="v">{fmt(remaining)}</span></div>
        <div className="kv-row"><span className="k">pct filled</span><span className="v">{icebergexecution ? `${pctFilled.toFixed(2)}%` : "—"}</span></div>
        <div className="kv-row"><span className="k">chunks placed</span><span className="v">{icebergexecution?.chunksPlaced ?? 0}</span></div>
        <div className="kv-row"><span className="k">chunks filled</span><span className="v accent">{icebergexecution?.chunksFilled ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CURRENT CHUNK</div></div>
        {cur ? (
          <>
            <div className="kv-row"><span className="k">seq</span><span className="v">#{cur.seq}</span></div>
            <div className="kv-row"><span className="k">qty</span><span className="v">{fmt(cur.qty)}</span></div>
            <div className="kv-row"><span className="k">side</span><span className={`v ${cur.side === "BID" ? "positive" : "negative"}`}>{cur.side}</span></div>
            <div className="kv-row"><span className="k">status</span><span className="v">{cur.status}</span></div>
            <div className="kv-row"><span className="k">at price</span><span className="v accent">{fmt(cur.price)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">seq</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">qty</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">status</span><span className="v faint">idle</span></div>
            <div className="kv-row"><span className="k">at price</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TIMING</div></div>
        <div className="kv-row"><span className="k">started at</span><span className="v">{startedAt ? new Date(startedAt).toLocaleTimeString() : "—"}</span></div>
        <div className="kv-row"><span className="k">elapsed s</span><span className="v">{icebergexecution ? elapsedSecs : "—"}</span></div>
        <div className="kv-row"><span className="k">max runtime s</span><span className="v">{c?.maxRuntimeSecs ?? "—"}</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(icebergexecution?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
