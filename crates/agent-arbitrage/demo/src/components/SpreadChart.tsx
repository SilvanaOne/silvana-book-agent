'use client';

import { useMemo, useState } from 'react';
import type { SpreadDto } from '@/lib/api';
import { CHART_PAIR_KEYS, demoSpreads } from '@/lib/demo';

/** Build {x,y} points for one pair, sorted by time, scaled into the viewBox. */
export interface ChartPoint {
  readonly x: number;
  readonly y: number;
  readonly bps: number;
  readonly ts: number;
}

const W = 640;
const H = 200;
const PAD = { top: 16, right: 14, bottom: 22, left: 40 };

export function buildSeries(spreads: readonly SpreadDto[], pairKey: string): ChartPoint[] {
  const rows = spreads
    .filter((s) => s.basePairKey === pairKey)
    .map((s) => ({ ts: new Date(s.ts).getTime(), bps: s.spreadBps }))
    .sort((a, b) => a.ts - b.ts);
  if (rows.length === 0) return [];

  const tMin = rows[0]!.ts;
  const tMax = rows[rows.length - 1]!.ts;
  const bpsVals = rows.map((r) => r.bps);
  const yMin = Math.min(...bpsVals, 0);
  const yMax = Math.max(...bpsVals, 1);
  const tSpan = tMax - tMin || 1;
  const ySpan = yMax - yMin || 1;

  const innerW = W - PAD.left - PAD.right;
  const innerH = H - PAD.top - PAD.bottom;

  return rows.map((r) => ({
    x: PAD.left + ((r.ts - tMin) / tSpan) * innerW,
    y: PAD.top + innerH - ((r.bps - yMin) / ySpan) * innerH,
    bps: r.bps,
    ts: r.ts,
  }));
}

/**
 * Dashboard spread chart — always renders the fixed demo time-series.
 *
 * The live SSE feed is intentionally NOT plotted here: mock scanner output only
 * covers a subset of pairs (no CBTC/CETH in the dropdown) and its tick cadence
 * produces artificial zig-zags. Recent live rows still appear in the table below.
 */
export function SpreadChart({ spreads: _liveSpreads }: { spreads: readonly SpreadDto[] }) {
  const source = useMemo(() => demoSpreads(), []);
  const pairs = CHART_PAIR_KEYS;

  const [selected, setSelected] = useState<string | null>(null);
  const pairKey = selected && pairs.includes(selected as (typeof CHART_PAIR_KEYS)[number])
    ? selected
    : pairs[0] ?? null;

  const points = useMemo(
    () => (pairKey ? buildSeries(source, pairKey) : []),
    [source, pairKey],
  );

  const last = points[points.length - 1];
  const maxBps = points.length ? Math.max(...points.map((p) => p.bps)) : 0;

  const linePath = points.map((p, i) => `${i === 0 ? 'M' : 'L'} ${p.x.toFixed(1)} ${p.y.toFixed(1)}`).join(' ');
  const areaPath =
    points.length >= 2
      ? `${linePath} L ${points[points.length - 1]!.x.toFixed(1)} ${H - PAD.bottom} L ${points[0]!.x.toFixed(1)} ${H - PAD.bottom} Z`
      : '';

  return (
    <div className="card-flush section" data-testid="spread-chart">
      <div className="card-title" style={{ marginBottom: '0.9rem' }}>
        <span style={{ display: 'inline-flex', alignItems: 'center', gap: '0.45rem' }}>
          Spread over time · bps
          <span className="badge" data-demo>
            demo
          </span>
        </span>
        {pairs.length > 0 ? (
          <select
            className="input"
            aria-label="pair"
            value={pairKey ?? ''}
            onChange={(e) => setSelected(e.target.value)}
            style={{ width: 'auto', fontSize: '0.75rem', padding: '0.25rem 0.5rem' }}
          >
            {pairs.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        ) : null}
      </div>

      {points.length < 2 ? (
        <p className="empty">collecting data — need at least two ticks for the selected pair</p>
      ) : (
        <>
          <svg
            viewBox={`0 0 ${W} ${H}`}
            width="100%"
            preserveAspectRatio="none"
            role="img"
            aria-label={`spread chart for ${pairKey}`}
            style={{ display: 'block' }}
          >
            <defs>
              <linearGradient id="spreadFill" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="rgba(255,122,26,0.35)" />
                <stop offset="100%" stopColor="rgba(255,122,26,0)" />
              </linearGradient>
              <linearGradient id="spreadLine" x1="0" y1="0" x2="1" y2="0">
                <stop offset="0%" stopColor="#ff7a1a" />
                <stop offset="100%" stopColor="#ffa752" />
              </linearGradient>
            </defs>

            {/* baseline */}
            <line
              x1={PAD.left}
              y1={H - PAD.bottom}
              x2={W - PAD.right}
              y2={H - PAD.bottom}
              stroke="var(--border)"
              strokeWidth="1"
            />
            {/* y labels */}
            <text x="6" y={PAD.top + 4} fill="var(--text-mute)" fontSize="11" fontFamily="var(--font-mono)">
              {maxBps}
            </text>
            <text x="6" y={H - PAD.bottom} fill="var(--text-mute)" fontSize="11" fontFamily="var(--font-mono)">
              0
            </text>

            {areaPath ? <path d={areaPath} fill="url(#spreadFill)" /> : null}
            <path d={linePath} fill="none" stroke="url(#spreadLine)" strokeWidth="2.5" strokeLinejoin="round" />
            {last ? <circle cx={last.x} cy={last.y} r="3.5" fill="#ffa752" /> : null}
          </svg>

          <div className="statusline" style={{ marginTop: '0.6rem', justifyContent: 'space-between' }}>
            <span className="mute">{points.length} ticks</span>
            <span>
              <span className="mute">latest</span>{' '}
              <span className="mono accent-text" style={{ fontWeight: 700 }}>
                {last?.bps ?? 0} bps
              </span>
            </span>
            <span>
              <span className="mute">peak</span> <span className="mono">{maxBps} bps</span>
            </span>
          </div>
        </>
      )}
    </div>
  );
}
