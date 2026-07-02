"use client";

import type { EventEntry } from "@/lib/store";

type Props = Readonly<{ events: readonly EventEntry[] }>;

function ts(t: number): string {
  const d = new Date(t);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function EventsLog({ events }: Props) {
  if (events.length === 0) {
    return <div className="muted">No events yet.</div>;
  }
  const items = [...events].reverse();
  return (
    <div className="mono" style={{ fontSize: 12, lineHeight: 1.6, maxHeight: 260, overflow: "auto" }}>
      {items.map((e, i) => {
        // Colour hint: try to parse RSI= value and colour the row on overbought/oversold.
        let color = "#b8c5df";
        const rsiMatch = e.message.match(/RSI=(\d+(?:\.\d+)?)/);
        if (rsiMatch) {
          const rsi = Number(rsiMatch[1]);
          if (rsi >= 70) color = "#ff8dab";
          else if (rsi <= 30) color = "#6de6a3";
        }
        if (e.message.startsWith("TA started")) color = "#c299ff";
        if (e.message.includes("stopped")) color = "#ffd166";
        return (
          <div key={i} style={{ display: "flex", gap: 10, borderBottom: "1px solid #1c2647", padding: "4px 0" }}>
            <span className="muted" style={{ minWidth: 60 }}>{ts(e.t)}</span>
            <span style={{ color }}>{e.message}</span>
          </div>
        );
      })}
    </div>
  );
}
