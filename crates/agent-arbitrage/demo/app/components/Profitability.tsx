"use client";

import { venueColor, venueName } from "@/lib/demo-data";
import type { ArbitrageState } from "@/lib/arbitrage-engine";

type Props = Readonly<{ arbitrage: ArbitrageState | null }>;

export function Profitability({ arbitrage }: Props) {
  if (!arbitrage) {
    return <div className="muted">No data — start the scanner to accumulate profitability stats.</div>;
  }

  const venues = [...arbitrage.byVenue].sort((a, b) => b.count - a.count);
  const maxVenue = Math.max(1, ...venues.map((v) => v.count));
  const buckets = arbitrage.bySize;
  const maxBucket = Math.max(1, ...buckets.map((b) => b.count));

  return (
    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 24 }}>
      {/* Per-venue participation. */}
      <div>
        <div className="mono" style={{ fontSize: 12, color: "var(--text-faint)", marginBottom: 8 }}>
          Venue participation (buy + sell legs)
        </div>
        <div className="stack" style={{ gap: 7 }}>
          {venues.map((v) => {
            const w = (v.count / maxVenue) * 100;
            return (
              <div key={v.venueId} style={{ display: "grid", gridTemplateColumns: "78px 1fr 96px", alignItems: "center", gap: 8 }}>
                <span className="mono" style={{ fontSize: 12, color: "var(--text)" }}>{venueName(v.venueId)}</span>
                <div style={{ background: "#1a1a22", borderRadius: 4, height: 14, position: "relative" }}>
                  <div style={{ width: `${w}%`, height: "100%", background: venueColor(v.venueId), opacity: 0.85, borderRadius: 4 }} />
                </div>
                <span className="mono faint" style={{ fontSize: 11, textAlign: "right" }}>
                  {v.count} · ${v.profitUsd.toFixed(2)}
                </span>
              </div>
            );
          })}
        </div>
      </div>

      {/* Spread-size histogram. */}
      <div>
        <div className="mono" style={{ fontSize: 12, color: "var(--text-faint)", marginBottom: 8 }}>
          Spread-size distribution (bps buckets)
        </div>
        <div className="stack" style={{ gap: 7 }}>
          {buckets.map((b) => {
            const w = (b.count / maxBucket) * 100;
            return (
              <div key={b.label} style={{ display: "grid", gridTemplateColumns: "72px 1fr 40px", alignItems: "center", gap: 8 }}>
                <span className="mono" style={{ fontSize: 12, color: "var(--text-muted)" }}>{b.label}</span>
                <div style={{ background: "#1a1a22", borderRadius: 4, height: 14 }}>
                  <div style={{ width: `${w}%`, height: "100%", background: "var(--accent)", opacity: 0.8, borderRadius: 4 }} />
                </div>
                <span className="mono faint" style={{ fontSize: 11, textAlign: "right" }}>{b.count}</span>
              </div>
            );
          })}
        </div>
        <div className="row" style={{ gap: 18, marginTop: 14, flexWrap: "wrap" }}>
          <div>
            <div className="mono faint" style={{ fontSize: 10.5, textTransform: "uppercase", letterSpacing: 1 }}>total spreads</div>
            <div className="mono" style={{ fontSize: 18 }}>{arbitrage.spreadsFound}</div>
          </div>
          <div>
            <div className="mono faint" style={{ fontSize: 10.5, textTransform: "uppercase", letterSpacing: 1 }}>est. profit Σ</div>
            <div className="mono accent" style={{ fontSize: 18, color: "var(--accent)" }}>${arbitrage.estProfitUsd.toFixed(2)}</div>
          </div>
          <div>
            <div className="mono faint" style={{ fontSize: 10.5, textTransform: "uppercase", letterSpacing: 1 }}>max spread</div>
            <div className="mono positive" style={{ fontSize: 18, color: "var(--pos)" }}>{arbitrage.bestBps} bps</div>
          </div>
        </div>
      </div>
    </div>
  );
}
