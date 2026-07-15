'use client';

import { useState, type ReactNode } from 'react';

export interface Datum {
  readonly label: string;
  readonly value: number;
  readonly color?: string;
  readonly sub?: string;
}

interface TipState {
  readonly x: number;
  readonly y: number;
  readonly node: ReactNode;
}

function useTip() {
  const [tip, setTip] = useState<TipState | null>(null);
  const show = (e: { clientX: number; clientY: number }, node: ReactNode) =>
    setTip({ x: e.clientX, y: e.clientY, node });
  const hide = () => setTip(null);
  const el = tip ? (
    <div className="chart-tip" style={{ left: tip.x, top: tip.y }}>
      {tip.node}
    </div>
  ) : null;
  return { show, hide, el };
}

/** Horizontal bars — e.g. profitable spreads by venue. Bars scale to the max. */
export function HBarChart({ data, unit = '' }: { data: readonly Datum[]; unit?: string }) {
  const { show, hide, el } = useTip();
  const max = Math.max(1, ...data.map((d) => d.value));
  if (data.length === 0) return <p className="empty">no data yet</p>;
  return (
    <div className="hbars">
      {data.map((d) => (
        <div className="hbar-row" key={d.label}>
          <span className="hbar-label">
            {d.color ? <i className="dot" style={{ background: d.color }} /> : null}
            {d.label}
          </span>
          <div className="hbar-track">
            <div
              className="hbar-fill"
              style={{ width: `${(d.value / max) * 100}%`, background: d.color ?? 'var(--accent-grad)' }}
              onMouseMove={(e) =>
                show(e, (
                  <>
                    {d.label}: <b>{d.value}{unit}</b>
                    {d.sub ? <span className="mute"> · {d.sub}</span> : null}
                  </>
                ))
              }
              onMouseLeave={hide}
            />
          </div>
          <span className="hbar-val">{d.value}</span>
        </div>
      ))}
      {el}
    </div>
  );
}

/** Vertical histogram — e.g. spreads bucketed by size (small → large). */
export function Histogram({ data, unit = '' }: { data: readonly Datum[]; unit?: string }) {
  const { show, hide, el } = useTip();
  const max = Math.max(1, ...data.map((d) => d.value));
  return (
    <>
      <div className="histo">
        {data.map((d) => (
          <div className="histo-col" key={d.label}>
            <div
              className="histo-bar"
              style={{ height: `${(d.value / max) * 100}%` }}
              onMouseMove={(e) =>
                show(e, (
                  <>
                    {d.label}{unit}: <b>{d.value}</b>
                  </>
                ))
              }
              onMouseLeave={hide}
            />
            <span className="histo-x">{d.label}</span>
          </div>
        ))}
      </div>
      {el}
    </>
  );
}

// ── donut ─────────────────────────────────────────────────────────────────

export function polar(cx: number, cy: number, r: number, angleDeg: number): [number, number] {
  const a = ((angleDeg - 90) * Math.PI) / 180;
  return [cx + r * Math.cos(a), cy + r * Math.sin(a)];
}

export function donutPath(
  cx: number,
  cy: number,
  R: number,
  r: number,
  start: number,
  end: number,
): string {
  const large = end - start > 180 ? 1 : 0;
  const [x1, y1] = polar(cx, cy, R, end);
  const [x2, y2] = polar(cx, cy, R, start);
  const [x3, y3] = polar(cx, cy, r, start);
  const [x4, y4] = polar(cx, cy, r, end);
  return `M ${x1.toFixed(2)} ${y1.toFixed(2)} A ${R} ${R} 0 ${large} 0 ${x2.toFixed(2)} ${y2.toFixed(2)} L ${x3.toFixed(2)} ${y3.toFixed(2)} A ${r} ${r} 0 ${large} 1 ${x4.toFixed(2)} ${y4.toFixed(2)} Z`;
}

export interface Segment extends Datum {
  readonly start: number;
  readonly end: number;
  readonly pct: number;
}

/** Compute pie/donut segments (angles + share) from data values. */
export function buildSegments(data: readonly Datum[]): Segment[] {
  const total = data.reduce((s, d) => s + d.value, 0);
  if (total <= 0) return [];
  let angle = 0;
  return data
    .filter((d) => d.value > 0)
    .map((d) => {
      const sweep = (d.value / total) * 360;
      const seg: Segment = { ...d, start: angle, end: angle + sweep, pct: d.value / total };
      angle += sweep;
      return seg;
    });
}

export function Donut({ data, centerLabel }: { data: readonly Datum[]; centerLabel?: string }) {
  const { show, hide, el } = useTip();
  const segs = buildSegments(data);
  if (data.length === 0) return <p className="empty">no data yet</p>;
  const total = data.reduce((s, d) => s + d.value, 0);
  // map from label → segment for legend lookup (zero-value entries are absent)
  const segByLabel = new Map(segs.map((s) => [s.label, s] as const));
  return (
    <div>
      <div style={{ display: 'flex', gap: '1rem', alignItems: 'center', flexWrap: 'wrap' }}>
        <svg viewBox="0 0 120 120" width="150" height="150" role="img" aria-label="distribution donut">
          {segs.map((s) => (
            <path
              key={s.label}
              d={donutPath(60, 60, 54, 33, s.start, s.end)}
              fill={s.color ?? 'var(--accent)'}
              stroke="var(--donut-stroke)"
              strokeWidth="1.5"
              style={{ cursor: 'pointer' }}
              onMouseMove={(e) =>
                show(e, (
                  <>
                    {s.label}: <b>{(s.pct * 100).toFixed(1)}%</b>{' '}
                    <span className="mute">({s.value})</span>
                  </>
                ))
              }
              onMouseLeave={hide}
            />
          ))}
          <text x="60" y="56" textAnchor="middle" fontSize="20" fontFamily="var(--font-display)" fontWeight="700" fill="var(--text)">
            {total}
          </text>
          <text x="60" y="72" textAnchor="middle" fontSize="9" fontFamily="var(--font-mono)" fill="var(--text-mute)">
            {centerLabel ?? 'total'}
          </text>
        </svg>
        {/* Legend lists EVERY data entry — including zero-value ones — so the
            full venue / coin catalogue stays visible even when something has
            no spread participation yet. */}
        <div className="legend" style={{ flexDirection: 'column', margin: 0, gap: '0.35rem' }}>
          {data.map((d) => {
            const seg = segByLabel.get(d.label);
            const pct = seg ? (seg.pct * 100).toFixed(0) : '0';
            return (
              <span key={d.label} style={!seg ? { opacity: 0.55 } : undefined}>
                <i style={{ background: d.color ?? 'var(--accent)' }} />
                {d.label} <span className="mute">· {pct}%</span>
              </span>
            );
          })}
        </div>
      </div>
      {el}
    </div>
  );
}
