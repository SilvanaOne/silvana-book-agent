"use client";

import type { Tick } from "@/lib/store";
import type { ReadinessCheckState, CheckResult } from "@/lib/readinesscheck-engine";

type Props = Readonly<{ ticks: readonly Tick[]; readinesscheck: ReadinessCheckState | null }>;

const W = 720;

export function ReadinessCheckChart({ ticks, readinesscheck }: Props) {
  if (!readinesscheck) {
    return (
      <div style={{ height: 300, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start Readiness Check to see the checklist and readiness timeline.
      </div>
    );
  }

  const checks = readinesscheck.checks;
  const rowH = 26;
  const listPadTop = 18;
  const listH = Math.max(1, checks.length) * rowH + listPadTop + 8;
  const tlPadTop = listH + 34; // title spacing
  const tlH = 90;
  const totalH = tlPadTop + tlH + 10;

  // Timeline geometry — ready(1) / not-ready(0) line
  const readyTicks = ticks.filter((t) => t.ready !== null);
  const tlLeft = 60, tlRight = W - 20;
  let tlPath = "";
  let segments: { d: string; ok: boolean }[] = [];
  if (readyTicks.length > 0) {
    const tMin = readyTicks[0].t, tMax = readyTicks[readyTicks.length - 1].t;
    const tRange = tMax - tMin || 1;
    const x = (t: number) => tlLeft + ((t - tMin) / tRange) * (tlRight - tlLeft);
    const y = (v: boolean) => tlPadTop + (v ? 20 : 60);
    // Build segments per-tick so consecutive ready ticks share color
    let curOk: boolean | null = null;
    let d = "";
    for (const t of readyTicks) {
      const ok = t.ready === true;
      const px = x(t.t).toFixed(1);
      const py = y(ok).toFixed(1);
      if (curOk === null) {
        d = `M${px},${py}`;
        curOk = ok;
      } else if (ok === curOk) {
        d += ` L${px},${py}`;
      } else {
        // step at boundary
        const stepY = y(curOk).toFixed(1);
        d += ` L${px},${stepY}`;
        segments.push({ d, ok: curOk });
        d = `M${px},${stepY} L${px},${py}`;
        segments.push({ d, ok: curOk });
        d = `M${px},${py}`;
        curOk = ok;
      }
    }
    if (curOk !== null && d) segments.push({ d, ok: curOk });
    tlPath = d;
  }

  return (
    <svg viewBox={`0 0 ${W + 90} ${totalH}`} style={{ width: "100%", height: "auto", maxHeight: 460 }}>
      <text x={20} y={14} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">CHECKLIST ({checks.length})</text>
      {checks.length === 0 && (
        <text x={20} y={40} fill="var(--text-faint)" fontSize={12} fontFamily="ui-monospace, monospace">
          waiting for first check…
        </text>
      )}
      {checks.map((c: CheckResult, i: number) => {
        const y = listPadTop + i * rowH + 14;
        const color = c.ok ? "var(--pos)" : "var(--neg)";
        const glyph = c.ok ? "✓" : "✗";
        return (
          <g key={i}>
            <rect x={16} y={y - 14} width={W + 60} height={rowH - 4} fill={c.ok ? "rgba(46,160,67,0.08)" : "rgba(229,83,75,0.08)"} rx={4} />
            <text x={26} y={y + 3} fill={color} fontSize={16} fontFamily="ui-monospace, monospace" fontWeight="bold">{glyph}</text>
            <text x={48} y={y + 3} fill="var(--text)" fontSize={12} fontFamily="ui-monospace, monospace">{c.name}</text>
            <text x={260} y={y + 3} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">current: {String(c.current ?? "—")}</text>
            <text x={430} y={y + 3} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">threshold: {String(c.threshold ?? "—")}</text>
            <text x={620} y={y + 3} fill={color} fontSize={11} fontFamily="ui-monospace, monospace">{c.ok ? "OK" : "FAIL"}</text>
          </g>
        );
      })}

      {/* Timeline */}
      <text x={20} y={tlPadTop - 12} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">READY TIMELINE</text>
      <line x1={tlLeft} x2={tlRight} y1={tlPadTop + 20} y2={tlPadTop + 20} stroke="#22222c" strokeWidth={1} strokeDasharray="2,2" />
      <line x1={tlLeft} x2={tlRight} y1={tlPadTop + 60} y2={tlPadTop + 60} stroke="#22222c" strokeWidth={1} strokeDasharray="2,2" />
      <text x={tlLeft - 8} y={tlPadTop + 24} fill="var(--pos)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">READY</text>
      <text x={tlLeft - 8} y={tlPadTop + 64} fill="var(--neg)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">FAIL</text>

      {segments.length === 0 && tlPath && (
        <path d={tlPath} stroke={readinesscheck.overallReady ? "var(--pos)" : "var(--neg)"} strokeWidth={2} fill="none" />
      )}
      {segments.map((s, i) => (
        <path key={i} d={s.d} stroke={s.ok ? "var(--pos)" : "var(--neg)"} strokeWidth={2} fill="none" />
      ))}
    </svg>
  );
}
