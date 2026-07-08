"use client";

import type { PreTradeCheckState, OrderCheck } from "@/lib/pretradecheck-engine";

type Props = Readonly<{ pretradecheck: PreTradeCheckState | null }>;

const W = 720, H = 320;

export function PreTradeCheckChart({ pretradecheck }: Props) {
  if (!pretradecheck) {
    return (
      <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>
        No data — start pre-trade-check to see decision analytics.
      </div>
    );
  }
  const stats = pretradecheck.stats;
  const checks = pretradecheck.checks;

  return (
    <div className="stack" style={{ gap: 20 }}>
      <AcceptRejectBar accepted={stats.accepted} rejected={stats.rejected} total={stats.total} />
      <FailedRuleBar byFailedRule={stats.byFailedRule} />
      <Scatter checks={checks} />
    </div>
  );
}

// ---- accepted vs rejected -------------------------------------------------

function AcceptRejectBar({ accepted, rejected, total }: { accepted: number; rejected: number; total: number }) {
  const maxV = Math.max(1, accepted, rejected);
  const bw = 220;
  const bh = 26;
  const w = (v: number) => Math.max(2, (v / maxV) * bw);

  return (
    <div>
      <div className="mono" style={{ color: "var(--text-faint)", fontSize: 11, marginBottom: 6 }}>
        DECISIONS ({total} checks)
      </div>
      <svg viewBox={`0 0 ${bw + 200} ${bh * 2 + 12}`} style={{ width: "100%", maxHeight: 90 }}>
        <text x={0} y={bh - 8} fill="var(--pos)" fontSize={11} fontFamily="ui-monospace, monospace">accept</text>
        <rect x={70} y={0} width={w(accepted)} height={bh} fill="var(--pos)" opacity={0.85} />
        <text x={78 + w(accepted)} y={bh - 8} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{accepted}</text>

        <text x={0} y={bh * 2 + 4} fill="var(--neg)" fontSize={11} fontFamily="ui-monospace, monospace">reject</text>
        <rect x={70} y={bh + 12} width={w(rejected)} height={bh} fill="var(--neg)" opacity={0.85} />
        <text x={78 + w(rejected)} y={bh * 2 + 4} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{rejected}</text>
      </svg>
    </div>
  );
}

// ---- breakdown by failed rule --------------------------------------------

function FailedRuleBar({ byFailedRule }: { byFailedRule: Record<string, number> }) {
  const entries = Object.entries(byFailedRule).sort(([, a], [, b]) => b - a);
  if (entries.length === 0) {
    return (
      <div>
        <div className="mono" style={{ color: "var(--text-faint)", fontSize: 11, marginBottom: 6 }}>FAILED RULES</div>
        <div className="muted mono" style={{ fontSize: 12 }}>no rule failures yet</div>
      </div>
    );
  }
  const maxV = Math.max(...entries.map(([, v]) => v));
  const bw = 220;
  const rowH = 20;
  const H2 = entries.length * (rowH + 4);

  return (
    <div>
      <div className="mono" style={{ color: "var(--text-faint)", fontSize: 11, marginBottom: 6 }}>FAILED RULES</div>
      <svg viewBox={`0 0 ${bw + 250} ${H2}`} style={{ width: "100%", maxHeight: 180 }}>
        {entries.map(([rule, count], i) => {
          const y = i * (rowH + 4);
          const w = Math.max(2, (count / maxV) * bw);
          return (
            <g key={rule}>
              <text x={0} y={y + rowH - 6} fill="var(--text-faint)" fontSize={11} fontFamily="ui-monospace, monospace">{rule}</text>
              <rect x={140} y={y} width={w} height={rowH - 4} fill="var(--accent)" opacity={0.85} />
              <text x={148 + w} y={y + rowH - 6} fill="var(--text)" fontSize={11} fontFamily="ui-monospace, monospace">{count}</text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}

// ---- scatter: price × qty coloured by decision ---------------------------

function Scatter({ checks }: { checks: readonly OrderCheck[] }) {
  if (checks.length === 0) {
    return (
      <div>
        <div className="mono" style={{ color: "var(--text-faint)", fontSize: 11, marginBottom: 6 }}>PRICE × QUANTITY</div>
        <div className="muted mono" style={{ fontSize: 12 }}>waiting for the first order…</div>
      </div>
    );
  }

  const PAD_L = 46, PAD_R = 12, PAD_T = 10, PAD_B = 24;
  const H2 = 200;
  const prices = checks.map((c) => c.price);
  const qtys = checks.map((c) => c.quantity);
  const xMin = Math.min(...prices);
  const xMax = Math.max(...prices);
  const yMin = Math.min(...qtys);
  const yMax = Math.max(...qtys);
  const xRange = xMax - xMin || 1;
  const yRange = yMax - yMin || 1;
  const x = (v: number) => PAD_L + ((v - xMin) / xRange) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_T + (1 - (v - yMin) / yRange) * (H2 - PAD_T - PAD_B);

  return (
    <div>
      <div className="mono" style={{ color: "var(--text-faint)", fontSize: 11, marginBottom: 6 }}>PRICE × QUANTITY (green = accept, red = reject)</div>
      <svg viewBox={`0 0 ${W} ${H2}`} style={{ width: "100%", maxHeight: 240 }}>
        {/* grid */}
        {[0, 0.25, 0.5, 0.75, 1].map((f, i) => (
          <g key={i}>
            <line x1={PAD_L} x2={W - PAD_R} y1={PAD_T + f * (H2 - PAD_T - PAD_B)} y2={PAD_T + f * (H2 - PAD_T - PAD_B)} stroke="#22222c" strokeWidth={1} />
            <text x={PAD_L - 6} y={PAD_T + f * (H2 - PAD_T - PAD_B) + 3} fill="var(--text-faint)" fontSize={9} textAnchor="end" fontFamily="ui-monospace, monospace">
              {(yMax - f * yRange).toFixed(1)}
            </text>
          </g>
        ))}
        {[0, 0.25, 0.5, 0.75, 1].map((f, i) => (
          <text key={`xl-${i}`} x={PAD_L + f * (W - PAD_L - PAD_R)} y={H2 - 6} fill="var(--text-faint)" fontSize={9} textAnchor="middle" fontFamily="ui-monospace, monospace">
            {(xMin + f * xRange).toFixed(2)}
          </text>
        ))}

        {/* axis labels */}
        <text x={PAD_L} y={PAD_T - 2} fill="var(--text-faint)" fontSize={9} fontFamily="ui-monospace, monospace">qty</text>
        <text x={W - PAD_R} y={H2 - 6} fill="var(--text-faint)" fontSize={9} textAnchor="end" fontFamily="ui-monospace, monospace">price</text>

        {/* points */}
        {checks.map((c) => (
          <circle
            key={c.seq}
            cx={x(c.price)}
            cy={y(c.quantity)}
            r={3.5}
            fill={c.accepted ? "var(--pos)" : "var(--neg)"}
            opacity={0.8}
          />
        ))}
      </svg>
    </div>
  );
}
