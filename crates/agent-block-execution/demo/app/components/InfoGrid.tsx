"use client";

import type { BlockExecutionState } from "@/lib/blockexecution-engine";

function fmt(n: number | null | undefined, min = 4): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? min : 6;
  return n.toFixed(digits);
}

function secs(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "—";
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const r = s - m * 60;
  return `${m}m${r.toString().padStart(2, "0")}s`;
}

function iso(ms: number | undefined): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleTimeString();
}

type Props = Readonly<{
  blockexecution: BlockExecutionState | null;
  walk: { driftPerTick: number; volPerTick: number };
  now: number;
}>;

export function InfoGrid({ blockexecution, walk, now }: Props) {
  const c = blockexecution?.config;
  const total = c?.total ?? 0;
  const filled = blockexecution?.totalFilled ?? 0;
  const remaining = Math.max(0, total - filled);
  const pctFilled = total > 0 ? (filled / total) * 100 : 0;
  const stateLabel =
    blockexecution === null
      ? "idle"
      : blockexecution.status === "completed"
      ? "completed"
      : blockexecution.status === "monitoring"
      ? "working"
      : "stopped";

  const startedAt = blockexecution?.startedAt;
  const scheduleEnd =
    startedAt && blockexecution ? startedAt + c!.timeSlices * blockexecution.sliceIntervalMs : undefined;
  const nextSliceAt =
    blockexecution && startedAt ? startedAt + blockexecution.currentSlice * blockexecution.sliceIntervalMs : undefined;
  const nextSliceIn =
    nextSliceAt && blockexecution && blockexecution.currentSlice < (c?.timeSlices ?? 0) ? Math.max(0, nextSliceAt - now) : 0;
  const elapsed = startedAt ? Math.max(0, now - startedAt) : 0;

  return (
    <div className="info-grid">
      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">RUNTIME</div></div>
        <div className="kv-row"><span className="k">strategy mode</span><span className="v">block-execution</span></div>
        <div className="kv-row"><span className="k">silvana host</span><span className="v">standalone-dev</span></div>
        <div className="kv-row"><span className="k">poll interval</span><span className="v">1000 ms</span></div>
        <div className="kv-row"><span className="k">state</span><span className="v">{stateLabel}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">BE CONFIG</div></div>
        {c ? (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v">{c.market}</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v accent">{c.side}</span></div>
            <div className="kv-row"><span className="k">total / price</span><span className="v">{fmt(c.total)} @ {fmt(c.price)}</span></div>
            <div className="kv-row"><span className="k">slices / duration</span><span className="v">{c.timeSlices} × {(c.durationSecs / c.timeSlices).toFixed(1)}s = {c.durationSecs}s</span></div>
            <div className="kv-row"><span className="k">visible / chunk</span><span className="v">{fmt(c.visible)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">market</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">side</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">total / price</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">slices / duration</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">visible / chunk</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PROGRESS</div></div>
        <div className="kv-row"><span className="k">parent filled</span><span className="v accent">{fmt(filled)} / {fmt(total)}</span></div>
        <div className="kv-row"><span className="k">remaining</span><span className="v">{fmt(remaining)}</span></div>
        <div className="kv-row"><span className="k">pct filled</span><span className="v">{pctFilled.toFixed(1)}%</span></div>
        <div className="kv-row"><span className="k">chunks placed / filled</span><span className="v">{blockexecution?.chunksPlaced ?? 0} / {blockexecution?.chunksFilled ?? 0}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">CURRENT SLICE</div></div>
        {blockexecution && c ? (
          <>
            <div className="kv-row"><span className="k">slice</span><span className="v accent">{blockexecution.currentSlice} / {c.timeSlices}</span></div>
            <div className="kv-row"><span className="k">qty / slice</span><span className="v">{fmt(blockexecution.sliceQty)}</span></div>
            <div className="kv-row"><span className="k">slice remaining</span><span className="v">{fmt(blockexecution.sliceRemaining)}</span></div>
            <div className="kv-row"><span className="k">next slice in</span><span className="v">{blockexecution.currentSlice >= c.timeSlices ? "final" : secs(nextSliceIn)}</span></div>
          </>
        ) : (
          <>
            <div className="kv-row"><span className="k">slice</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">qty / slice</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">slice remaining</span><span className="v faint">—</span></div>
            <div className="kv-row"><span className="k">next slice in</span><span className="v faint">—</span></div>
          </>
        )}
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">TIMING</div></div>
        <div className="kv-row"><span className="k">started</span><span className="v">{iso(startedAt)}</span></div>
        <div className="kv-row"><span className="k">elapsed</span><span className="v">{startedAt ? secs(elapsed) : "—"}</span></div>
        <div className="kv-row"><span className="k">expected end</span><span className="v">{iso(scheduleEnd)}</span></div>
        <div className="kv-row"><span className="k">completed at</span><span className="v">{iso(blockexecution?.completedAt)}</span></div>
      </div>

      <div className="info-card">
        <div className="info-card-head"><div className="info-card-title">PRICE SIM</div></div>
        <div className="kv-row"><span className="k">current mid</span><span className="v">{fmt(blockexecution?.currentPrice)}</span></div>
        <div className="kv-row"><span className="k">source</span><span className="v">GBM walk</span></div>
        <div className="kv-row"><span className="k">drift / tick</span><span className="v">{walk.driftPerTick}</span></div>
        <div className="kv-row"><span className="k">vol / tick</span><span className="v">{(walk.volPerTick * 100).toFixed(2)}%</span></div>
      </div>
    </div>
  );
}
