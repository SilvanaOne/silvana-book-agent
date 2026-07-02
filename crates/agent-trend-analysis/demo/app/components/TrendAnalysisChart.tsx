"use client";

import type { Tick } from "@/lib/store";
import type { TrendAnalysisState } from "@/lib/trendanalysis-engine";

type Props = Readonly<{ ticks: readonly Tick[]; trendanalysis: TrendAnalysisState | null }>;

const W = 720;
const PAD_L = 60, PAD_R = 90, PAD_T = 14;
const H_MAIN = 220;      // price + SMA + EMA + BB
const GAP = 8;
const H_RSI = 90;        // RSI subchart 0..100
const H = PAD_T + H_MAIN + GAP + H_RSI + 24;

export function TrendAnalysisChart({ ticks, trendanalysis }: Props) {
  if (ticks.length === 0 || !trendanalysis) {
    return <div style={{ height: H, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-faint)" }}>No data — start Trend Analysis to see mid, SMA, EMA, Bollinger bands and RSI.</div>;
  }

  const prices = ticks.map((t) => t.price);
  const smas = ticks.map((t) => t.sma).filter((v): v is number => v !== null);
  const emas = ticks.map((t) => t.ema).filter((v): v is number => v !== null);
  const uppers = ticks.map((t) => t.bollingerUpper).filter((v): v is number => v !== null);
  const lowers = ticks.map((t) => t.bollingerLower).filter((v): v is number => v !== null);

  const minAll = Math.min(...prices, ...smas, ...emas, ...(uppers.length ? uppers : []), ...(lowers.length ? lowers : []));
  const maxAll = Math.max(...prices, ...smas, ...emas, ...(uppers.length ? uppers : []), ...(lowers.length ? lowers : []));
  const pad = (maxAll - minAll) * 0.1 || minAll * 0.02;
  const yMin = minAll - pad, yMax = maxAll + pad, yRange = yMax - yMin || 1;

  const tMin = ticks[0].t, tMax = ticks[ticks.length - 1].t;
  const tRange = tMax - tMin || 1;

  const x = (t: number) => PAD_L + ((t - tMin) / tRange) * (W - PAD_L - PAD_R);

  // Main pane y (prices)
  const mainTop = PAD_T;
  const mainBot = PAD_T + H_MAIN;
  const yMain = (v: number) => mainTop + (1 - (v - yMin) / yRange) * (mainBot - mainTop);

  // RSI pane y (0..100)
  const rsiTop = mainBot + GAP;
  const rsiBot = rsiTop + H_RSI;
  const yRsi = (v: number) => rsiTop + (1 - v / 100) * (rsiBot - rsiTop);

  const pathFrom = <T,>(sel: (t: Tick) => T | null, y: (v: number) => number) => {
    let started = false;
    const cmds: string[] = [];
    for (const t of ticks) {
      const v = sel(t);
      if (v === null || typeof v !== "number" || !Number.isFinite(v)) continue;
      const px = x(t.t).toFixed(1);
      const py = y(v as number).toFixed(1);
      cmds.push(`${started ? "L" : "M"}${px},${py}`);
      started = true;
    }
    return cmds.join(" ");
  };

  const pricePath = pathFrom((t) => t.price, yMain);
  const smaPath = pathFrom((t) => t.sma, yMain);
  const emaPath = pathFrom((t) => t.ema, yMain);
  const upperPath = pathFrom((t) => t.bollingerUpper, yMain);
  const lowerPath = pathFrom((t) => t.bollingerLower, yMain);
  const rsiPath = pathFrom((t) => t.rsi, yRsi);

  // Bollinger band fill: build a closed polygon (upper forward, lower backward)
  let bbFill = "";
  const upperPts: Array<[number, number]> = [];
  const lowerPts: Array<[number, number]> = [];
  for (const t of ticks) {
    if (t.bollingerUpper !== null && Number.isFinite(t.bollingerUpper)) upperPts.push([x(t.t), yMain(t.bollingerUpper)]);
    if (t.bollingerLower !== null && Number.isFinite(t.bollingerLower)) lowerPts.push([x(t.t), yMain(t.bollingerLower)]);
  }
  if (upperPts.length > 1 && lowerPts.length > 1) {
    const up = upperPts.map(([px, py], i) => `${i === 0 ? "M" : "L"}${px.toFixed(1)},${py.toFixed(1)}`).join(" ");
    const down = [...lowerPts].reverse().map(([px, py]) => `L${px.toFixed(1)},${py.toFixed(1)}`).join(" ");
    bbFill = `${up} ${down} Z`;
  }

  const digits = yMax > 100 ? 2 : yMax > 1 ? 4 : 6;
  const yMainTicks: number[] = [];
  for (let i = 0; i <= 4; i++) yMainTicks.push(yMin + (yRange * i) / 4);

  const lastPrice = ticks[ticks.length - 1].price;
  const lastEma = ticks[ticks.length - 1].ema;
  const lastSma = ticks[ticks.length - 1].sma;
  const lastRsi = ticks[ticks.length - 1].rsi;

  return (
    <svg viewBox={`0 0 ${W + 10} ${H}`} style={{ width: "100%", height: "auto", maxHeight: 460 }}>
      <rect x={0} y={0} width={W + 10} height={H} fill="transparent" />

      {/* Main pane grid */}
      {yMainTicks.map((v, i) => (
        <g key={`gm-${i}`}>
          <line x1={PAD_L} x2={W - PAD_R} y1={yMain(v)} y2={yMain(v)} stroke="#22222c" strokeWidth={1} />
          <text x={PAD_L - 8} y={yMain(v) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">{v.toFixed(digits)}</text>
        </g>
      ))}

      {/* Bollinger band fill */}
      {bbFill && <path d={bbFill} fill="#3f5b8a" opacity={0.12} />}

      {/* Bollinger band edges */}
      {upperPath && <path d={upperPath} stroke="#3f5b8a" strokeWidth={1} strokeDasharray="3,3" fill="none" opacity={0.75} />}
      {lowerPath && <path d={lowerPath} stroke="#3f5b8a" strokeWidth={1} strokeDasharray="3,3" fill="none" opacity={0.75} />}

      {/* SMA (middle) */}
      {smaPath && <path d={smaPath} stroke="#ffd166" strokeWidth={1.4} fill="none" opacity={0.9} />}

      {/* EMA */}
      {emaPath && <path d={emaPath} stroke="var(--accent)" strokeWidth={1.6} fill="none" opacity={0.9} />}

      {/* Mid price */}
      <path d={pricePath} stroke="#ececf1" strokeWidth={1.6} fill="none" />

      {/* Last-price marker */}
      <circle cx={x(tMax)} cy={yMain(lastPrice)} r={3.5} fill="#ececf1" />

      {/* Right-margin main labels */}
      <text x={W - PAD_R + 6} y={yMain(lastPrice) + 3} fill="#ececf1" fontSize={10} fontFamily="ui-monospace, monospace">MID {lastPrice.toFixed(digits)}</text>
      {lastEma !== null && Number.isFinite(lastEma) && (
        <text x={W - PAD_R + 6} y={yMain(lastEma) + 3} fill="var(--accent)" fontSize={10} fontFamily="ui-monospace, monospace">EMA {lastEma.toFixed(digits)}</text>
      )}
      {lastSma !== null && Number.isFinite(lastSma) && (
        <text x={W - PAD_R + 6} y={yMain(lastSma) + 14} fill="#ffd166" fontSize={10} fontFamily="ui-monospace, monospace">SMA {lastSma.toFixed(digits)}</text>
      )}

      {/* Separator */}
      <line x1={PAD_L} x2={W - PAD_R} y1={rsiTop - GAP / 2} y2={rsiTop - GAP / 2} stroke="#1c2647" strokeWidth={1} />

      {/* RSI pane background zones */}
      <line x1={PAD_L} x2={W - PAD_R} y1={yRsi(70)} y2={yRsi(70)} stroke="#ff8dab" strokeWidth={0.8} strokeDasharray="3,3" opacity={0.6} />
      <line x1={PAD_L} x2={W - PAD_R} y1={yRsi(50)} y2={yRsi(50)} stroke="#22222c" strokeWidth={0.8} />
      <line x1={PAD_L} x2={W - PAD_R} y1={yRsi(30)} y2={yRsi(30)} stroke="#6de6a3" strokeWidth={0.8} strokeDasharray="3,3" opacity={0.6} />

      {/* RSI axis labels */}
      <text x={PAD_L - 8} y={yRsi(70) + 3} fill="#ff8dab" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">70</text>
      <text x={PAD_L - 8} y={yRsi(50) + 3} fill="var(--text-faint)" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">50</text>
      <text x={PAD_L - 8} y={yRsi(30) + 3} fill="#6de6a3" fontSize={10} textAnchor="end" fontFamily="ui-monospace, monospace">30</text>

      {/* RSI line */}
      {rsiPath && <path d={rsiPath} stroke="#c299ff" strokeWidth={1.5} fill="none" />}

      {/* RSI current marker */}
      {lastRsi !== null && Number.isFinite(lastRsi) && (
        <>
          <circle cx={x(tMax)} cy={yRsi(lastRsi)} r={3} fill="#c299ff" />
          <text x={W - PAD_R + 6} y={yRsi(lastRsi) + 3} fill="#c299ff" fontSize={10} fontFamily="ui-monospace, monospace">RSI {lastRsi.toFixed(1)}</text>
        </>
      )}

      {/* Bottom label */}
      <text x={PAD_L} y={H - 6} fill="var(--text-faint)" fontSize={10} fontFamily="ui-monospace, monospace">
        mid · SMA · EMA · Bollinger ± {trendanalysis.config.bollingerK}σ   |   RSI({trendanalysis.config.rsiPeriod})
      </text>
    </svg>
  );
}
