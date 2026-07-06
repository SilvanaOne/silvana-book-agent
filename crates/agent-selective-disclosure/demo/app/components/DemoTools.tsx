"use client";

import { useState } from "react";
import type { DiscloseState } from "@/lib/disclose-engine";

type Props = Readonly<{
  agent: DiscloseState | null;
  onReload: () => Promise<void>;
  onReFilter: () => Promise<void>;
  onVerify: () => Promise<void>;
  onTamper: (seq: number) => Promise<void>;
}>;

export function DemoTools({ agent, onReload, onReFilter, onVerify, onTamper }: Props) {
  const [tamperSeq, setTamperSeq] = useState("");
  if (!agent) return <div className="muted">Load a history to enable demo tools.</div>;

  return (
    <div className="stack">
      <h3>Batch ops</h3>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <button className="ghost" onClick={onReload}>Reload history</button>
        <button className="ghost" onClick={onReFilter}>Re-filter</button>
        <button className="primary" onClick={onVerify}>Verify chain</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Tamper (then Verify)</h3>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" min={1} value={tamperSeq} onChange={(e) => setTamperSeq(e.target.value)} placeholder="seq" />
        <button className="danger" disabled={!tamperSeq || !Number.isInteger(Number(tamperSeq))} onClick={() => { onTamper(Number(tamperSeq)); setTamperSeq(""); }}>Tamper</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5 }}>
        Editing any record breaks the prev_hash chain from that record onwards. <span className="mono">Verify chain</span> will report the first broken seq.
      </div>
    </div>
  );
}
