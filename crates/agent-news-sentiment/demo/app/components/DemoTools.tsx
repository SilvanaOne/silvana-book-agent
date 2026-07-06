"use client";

import { useState } from "react";
import type { NewsState } from "@/lib/news-engine";

type Props = Readonly<{ agent: NewsState | null; onIngest: (text: string) => Promise<void> }>;

export function DemoTools({ agent, onIngest }: Props) {
  const [text, setText] = useState("");
  if (!agent) return <div className="muted">Start scoring to enable demo tools.</div>;
  return (
    <div className="stack">
      <h3>Inject a headline</h3>
      <input value={text} onChange={(e) => setText(e.target.value)} placeholder="e.g. Massive BTC rally after breakthrough news" />
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <button className="primary" disabled={text.trim().length === 0} onClick={() => { onIngest(text.trim()); setText(""); }}>Score it</button>
        <button className="ghost" onClick={() => setText("Massive Bitcoin rally sends BTC to record high")}>+ BTC bull</button>
        <button className="ghost" onClick={() => setText("Canton network approved by regulator")}>+ CC bull</button>
        <button className="ghost" onClick={() => setText("Ethereum plunges on SEC investigation, huge sell-off")}>+ ETH bear</button>
      </div>
    </div>
  );
}
