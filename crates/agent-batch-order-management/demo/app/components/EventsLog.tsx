"use client";

import type { EventEntry } from "@/lib/store";

type Props = Readonly<{ events: readonly EventEntry[] }>;

function ts(t: number): string {
  const d = new Date(t);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function colorFor(message: string): string {
  if (message.startsWith("COMPLETED") || message.startsWith("TRANSFER")) return "var(--pos)";
  if (message.startsWith("EXECUTE") || message.startsWith("RUNNING") || message.startsWith("PREVIEW")) return "var(--accent)";
  if (message.startsWith("DRIFT") || message.startsWith("THRESHOLD")) return "var(--neg)";
  return "var(--text)";
}

export function EventsLog({ events }: Props) {
  if (events.length === 0) {
    return <div className="muted">No events yet.</div>;
  }
  const items = [...events].reverse();
  return (
    <div className="mono" style={{ fontSize: 12, lineHeight: 1.6, maxHeight: 320, overflow: "auto" }}>
      {items.map((e, i) => (
        <div key={i} style={{ display: "flex", gap: 10, borderBottom: "1px dashed var(--border)", padding: "4px 0" }}>
          <span className="muted" style={{ minWidth: 60 }}>{ts(e.t)}</span>
          <span style={{ color: colorFor(e.message) }}>{e.message}</span>
        </div>
      ))}
    </div>
  );
}
