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
        const isProfit = e.message.includes("TAKE PROFIT");
        const isLoss = e.message.includes("STOP LOSS");
        const color = isProfit ? "#6de6a3" : isLoss ? "#ff8dab" : "#b8c5df";
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
