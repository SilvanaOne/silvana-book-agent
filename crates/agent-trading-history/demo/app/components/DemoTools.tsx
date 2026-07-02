"use client";

import { useMemo, useState } from "react";
import type { TradingHistoryState } from "@/lib/tradinghistory-engine";

type Props = Readonly<{
  tradinghistory: TradingHistoryState | null;
  onJump: (to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
  onTamper: (index: number) => Promise<void>;
}>;

export function DemoTools({ tradinghistory, onJump, onWalk, onTamper }: Props) {
  const [manual, setManual] = useState("");
  const [drift, setDrift] = useState("0");
  const [vol, setVol] = useState("0.8");
  const [tamperIdx, setTamperIdx] = useState<string>("");
  const [tamperBusy, setTamperBusy] = useState(false);

  const options = useMemo(() => {
    if (!tradinghistory) return [];
    return tradinghistory.records.map((r, i) => ({
      idx: i,
      label: `#${r.seq} · ${r.kind} · ${r.hash.slice(0, 8)}${r.tampered ? " (already tampered)" : ""}`,
    }));
  }, [tradinghistory]);

  if (!tradinghistory || tradinghistory.status !== "monitoring") return <div className="muted">Start Trading History to enable demo tools.</div>;

  const price = tradinghistory.currentPrice;
  const nudge = (mult: number) => Number((price * mult).toFixed(8));

  return (
    <div className="stack">
      <h3>Nudge price</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onJump(nudge(1.005))}>+0.5%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.02))}>+2%</button>
        <button className="ghost" onClick={() => onJump(nudge(1.05))}>+5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.995))}>−0.5%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.98))}>−2%</button>
        <button className="ghost" onClick={() => onJump(nudge(0.95))}>−5%</button>
      </div>
      <div className="row" style={{ gap: 6 }}>
        <input type="number" step="any" placeholder="Manual price" value={manual} onChange={(e) => setManual(e.target.value)} />
        <button disabled={!manual || !Number.isFinite(Number(manual))} onClick={() => onJump(Number(manual))}>Set</button>
      </div>

      <h3 style={{ marginTop: 14 }}>Random walk params</h3>
      <div className="grid-2">
        <div>
          <label>Drift / tick</label>
          <input type="number" step="any" value={drift} onChange={(e) => setDrift(e.target.value)} onBlur={() => { const n = Number(drift); if (Number.isFinite(n)) onWalk({ driftPerTick: n }); }} />
        </div>
        <div>
          <label>Vol / tick (%)</label>
          <input type="number" step="any" value={vol} onChange={(e) => setVol(e.target.value)} onBlur={() => { const n = Number(vol); if (Number.isFinite(n) && n >= 0) onWalk({ volPerTick: n / 100 }); }} />
        </div>
      </div>

      <h3 style={{ marginTop: 14 }}>Tamper a record</h3>
      <div className="muted" style={{ fontSize: 12, marginBottom: 6 }}>
        Mutate the payload of one record without recomputing its hash. All subsequent records will fail verification.
      </div>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <select
          value={tamperIdx}
          onChange={(e) => setTamperIdx(e.target.value)}
          disabled={options.length === 0 || tamperBusy}
          style={{ flex: 1, minWidth: 220 }}
        >
          <option value="">Select record to tamper…</option>
          {options.map((o) => (
            <option key={o.idx} value={o.idx}>{o.label}</option>
          ))}
        </select>
        <button
          className="danger"
          disabled={tamperIdx === "" || tamperBusy}
          onClick={async () => {
            const idx = Number(tamperIdx);
            if (!Number.isInteger(idx)) return;
            setTamperBusy(true);
            try { await onTamper(idx); setTamperIdx(""); } finally { setTamperBusy(false); }
          }}
        >
          {tamperBusy ? "Tampering…" : "Tamper record"}
        </button>
      </div>
    </div>
  );
}
