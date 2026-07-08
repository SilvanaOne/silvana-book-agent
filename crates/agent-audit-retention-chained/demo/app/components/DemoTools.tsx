"use client";

import { useState } from "react";
import type { RetentionState } from "@/lib/retention-engine";

type Props = Readonly<{
  agent: RetentionState | null;
  onRotate: () => Promise<void>;
  onVerify: () => Promise<void>;
  onTamper: (num: number) => Promise<void>;
}>;

export function DemoTools({ agent, onRotate, onVerify, onTamper }: Props) {
  const [num, setNum] = useState("");
  if (!agent) return <div className="muted">Load a history to enable demo tools.</div>;

  return (
    <div className="stack">
      <h3>Batch ops</h3>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <button className="ghost" onClick={onRotate}>Re-rotate</button>
        <button className="primary" onClick={onVerify}>Verify chain</button>
      </div>
      <h3 style={{ marginTop: 14 }}>Tamper with a slice (then Verify)</h3>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" min={1} value={num} onChange={(e) => setNum(e.target.value)} placeholder="slice #" />
        <button className="danger" disabled={!num || !Number.isInteger(Number(num))} onClick={() => { onTamper(Number(num)); setNum(""); }}>Tamper</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5 }}>
        Editing any slice breaks the <span className="mono">prev_slice_hash</span> chain from the next slice on. Verify catches it at that seq.
      </div>
    </div>
  );
}
