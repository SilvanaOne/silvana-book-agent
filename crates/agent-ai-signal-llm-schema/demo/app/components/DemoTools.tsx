"use client";

import { useState } from "react";
import type { AiSignalState } from "@/lib/ai-signal-engine";

type Props = Readonly<{ agent: AiSignalState | null; onEmit: (prompt: string) => Promise<void> }>;

export function DemoTools({ agent, onEmit }: Props) {
  const [prompt, setPrompt] = useState("");
  if (!agent) return <div className="muted">Start predicting to enable demo tools.</div>;

  return (
    <div className="stack">
      <h3>Send a prompt to the mock LLM</h3>
      <textarea rows={3} placeholder="e.g. BTC will dump on the CPI release, high confidence"
        value={prompt} onChange={(e) => setPrompt(e.target.value)}
        style={{ width: "100%", fontFamily: "ui-monospace, monospace", fontSize: 12.5, padding: 8, background: "var(--bg-input, #0f0f14)", color: "inherit", border: "1px solid #2a2a34", borderRadius: 6, resize: "vertical" }} />
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <button className="primary" disabled={prompt.trim().length === 0} onClick={() => { onEmit(prompt.trim()); setPrompt(""); }}>Send prompt</button>
        <button className="ghost" onClick={() => setPrompt("CC is going to pump within the hour, strong confidence")}>+ CC bull</button>
        <button className="ghost" onClick={() => setPrompt("BTC will crash on bearish news, high confidence")}>+ BTC bear</button>
        <button className="ghost" onClick={() => setPrompt("maybe eth is up, low confidence")}>+ weak</button>
      </div>
      <div className="muted" style={{ fontSize: 12.5, lineHeight: 1.6 }}>
        The mock LLM scores bull/bear keywords and confidence cues. Signals below <span className="mono">min_confidence</span> are dropped (no signal file written).
      </div>
    </div>
  );
}
