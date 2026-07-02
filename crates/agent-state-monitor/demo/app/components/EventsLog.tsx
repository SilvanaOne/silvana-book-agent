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
        const isFill = e.message.includes(" filled ");
        const isSettled = e.message.includes(" settled ");
        const isCancel = e.message.includes(" cancelled ");
        const isFail = e.message.includes(" failed ");
        const isProposal = e.message.includes(" proposal ");
        const color = isFill ? "#3ddc84" : isSettled ? "#4aa3ff" : isFail ? "#e05cff" : isCancel ? "#ff5c5c" : isProposal ? "#f4a742" : "#b8c5df";
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
