"use client";

import { venueName, type SpreadDto } from "@/lib/demo-data";

type Props = Readonly<{ rows: readonly SpreadDto[] }>;

function clock(iso: string): string {
  const d = new Date(iso);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function RecentSpreads({ rows }: Props) {
  if (rows.length === 0) {
    return <div className="muted">No spreads yet — start the scanner.</div>;
  }
  const items = rows.slice(0, 12);
  return (
    <div style={{ overflowX: "auto" }}>
      <table className="spreads">
        <thead>
          <tr>
            <th>time</th>
            <th>pair</th>
            <th>route</th>
            <th className="num">bps</th>
            <th className="num">est. $</th>
            <th className="num">status</th>
          </tr>
        </thead>
        <tbody>
          {items.map((r) => (
            <tr key={r.id}>
              <td className="mono faint">{clock(r.ts)}</td>
              <td className="mono">{r.basePairKey}</td>
              <td className="mono route">
                <span className="v-buy">{venueName(r.buyVenueId)}</span>
                <span className="arrow"> → </span>
                <span className="v-sell">{venueName(r.sellVenueId)}</span>
              </td>
              <td className="num mono accent">{r.spreadBps}</td>
              <td className="num mono">{r.estProfitUsd}</td>
              <td className="num">
                {r.acted ? (
                  <span className="badge long">ACTED</span>
                ) : (
                  <span className="badge">seen</span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
