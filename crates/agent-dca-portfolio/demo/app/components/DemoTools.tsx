"use client";

import { useEffect, useState } from "react";
import type { DcaPortfolioState } from "@/lib/dcaportfolio-engine";

type Props = Readonly<{
  dcaportfolio: DcaPortfolioState | null;
  onJump: (market: string | undefined, to: number) => Promise<void>;
  onWalk: (patch: { driftPerTick?: number; volPerTick?: number }) => Promise<void>;
}>;

export function DemoTools({ dcaportfolio, onJump, onWalk }: Props) {
  const [manual, setManual] = useState("");
  const [drift, setDrift] = useState("0");
  const [vol, setVol] = useState("0.8");
  const [selectedMarket, setSelectedMarket] = useState<string>("");

  useEffect(() => {
    if (dcaportfolio && dcaportfolio.progress.length > 0 && !selectedMarket) {
      setSelectedMarket(dcaportfolio.progress[0].market);
    }
  }, [dcaportfolio, selectedMarket]);

  if (!dcaportfolio || dcaportfolio.status !== "running") {
    return <div className="muted">Start DCA Portfolio to enable demo tools.</div>;
  }

  const currentMarket = dcaportfolio.progress.find((mp) => mp.market === selectedMarket)
    ?? dcaportfolio.progress[0];
  const price = currentMarket.currentPrice;
  const nudge = (mult: number) => Number((price * mult).toFixed(8));

  return (
    <div className="stack">
      <h3>Target market</h3>
      <select
        value={selectedMarket || currentMarket.market}
        onChange={(e) => setSelectedMarket(e.target.value)}
      >
        {dcaportfolio.progress.map((mp) => (
          <option key={mp.market} value={mp.market}>
            {mp.market} (mid {mp.currentPrice.toFixed(6)})
          </option>
        ))}
      </select>

      <h3 style={{ marginTop: 10 }}>Nudge mid on {currentMarket.market}</h3>
      <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
        <button className="ghost" onClick={() => onJump(currentMarket.market, nudge(1.005))}>+0.5%</button>
        <button className="ghost" onClick={() => onJump(currentMarket.market, nudge(1.02))}>+2%</button>
        <button className="ghost" onClick={() => onJump(currentMarket.market, nudge(1.05))}>+5%</button>
        <button className="ghost" onClick={() => onJump(currentMarket.market, nudge(0.995))}>−0.5%</button>
        <button className="ghost" onClick={() => onJump(currentMarket.market, nudge(0.98))}>−2%</button>
        <button className="ghost" onClick={() => onJump(currentMarket.market, nudge(0.95))}>−5%</button>
      </div>
      <div className="row" style={{ gap: 6 }}>
        <input
          type="number"
          step="any"
          placeholder="Manual price"
          value={manual}
          onChange={(e) => setManual(e.target.value)}
        />
        <button
          disabled={!manual || !Number.isFinite(Number(manual))}
          onClick={() => onJump(currentMarket.market, Number(manual))}
        >
          Set
        </button>
      </div>

      <h3 style={{ marginTop: 14 }}>Random walk params (applied to all markets)</h3>
      <div className="grid-2">
        <div>
          <label>Drift / tick</label>
          <input
            type="number"
            step="any"
            value={drift}
            onChange={(e) => setDrift(e.target.value)}
            onBlur={() => {
              const n = Number(drift);
              if (Number.isFinite(n)) onWalk({ driftPerTick: n });
            }}
          />
        </div>
        <div>
          <label>Vol / tick (%)</label>
          <input
            type="number"
            step="any"
            value={vol}
            onChange={(e) => setVol(e.target.value)}
            onBlur={() => {
              const n = Number(vol);
              if (Number.isFinite(n) && n >= 0) onWalk({ volPerTick: n / 100 });
            }}
          />
        </div>
      </div>
    </div>
  );
}
