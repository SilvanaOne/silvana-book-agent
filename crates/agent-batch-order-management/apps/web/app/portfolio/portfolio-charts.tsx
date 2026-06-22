"use client";

import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Legend,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

// Diverse palette across hues so additional assets stay visually distinct from each other.
const PALETTE = [
  "#86b2ff", // sky blue
  "#fb9b4b", // orange
  "#b45309", // brown
  "#ef4444", // red
  "#facc15", // yellow
  "#10b981", // green
  "#be98ff", // light purple
  "#7b6ef6", // deep purple
  "#fb7185", // pink
  "#0ea5e9", // cyan
];

/** Bar chart: Target column (all) — light salad green */
const BAR_TARGET_SALAD = "#c5eec5";

// Per-asset overrides so the major tickers always render in a recognizable colour.
const ASSET_COLOR: Record<string, string> = {
  USDC: "#86b2ff", // sky blue
  CC: "#fb9b4b", // orange (unchanged)
  WBTC: "#b45309", // brown / bitcoin-gold
  WETH: "#ef4444", // red
};

function pieSliceFill(assetSymbol: string, index: number): string {
  return ASSET_COLOR[assetSymbol] ?? PALETTE[index % PALETTE.length] ?? "#be98ff";
}

function actualBarFill(assetSymbol: string, index: number): string {
  return ASSET_COLOR[assetSymbol] ?? PALETTE[index % PALETTE.length] ?? "#be98ff";
}

function parseWt(s: string): number {
  const n = Number(s);
  return Number.isFinite(n) ? n : 0;
}

export function PortfolioCharts(props: {
  currentWeights: ReadonlyArray<{ assetSymbol: string; weight: string }>;
  drift: ReadonlyArray<{
    assetSymbol: string;
    targetWeight: string;
    currentWeight: string;
  }>;
}) {
  const pieData = props.currentWeights
    .map((w) => ({
      name: w.assetSymbol,
      value: Math.max(0, parseWt(w.weight)),
    }))
    .filter((d) => d.value > 0);

  const barData = props.drift.map((d) => ({
    name: d.assetSymbol,
    target: parseWt(d.targetWeight) * 100,
    actual: parseWt(d.currentWeight) * 100,
  }));

  if (pieData.length === 0 && barData.length === 0) {
    return null;
  }

  return (
    <section className="portfolio-charts" aria-label="Portfolio visualization">
      <h2>Allocation</h2>

      <div className="portfolio-charts__stack">
        {pieData.length > 0 ? (
          <div className="silv-chart-card silv-card silv-card--venue portfolio-charts__donut-card">
            <h3 className="portfolio-charts__subtitle">Current mix</h3>
            <p className="portfolio-charts__hint muted">Share of portfolio value by asset</p>
            <div className="portfolio-charts__donut">
              <ResponsiveContainer width="100%" height={300}>
                <PieChart>
                  <Pie
                    data={pieData}
                    dataKey="value"
                    nameKey="name"
                    cx="50%"
                    cy="50%"
                    innerRadius="52%"
                    outerRadius="78%"
                    paddingAngle={2.5}
                    stroke="rgba(255,255,255,0.92)"
                    strokeWidth={2}
                  >
                    {pieData.map((d, i) => (
                      <Cell key={`cell-${d.name}`} fill={pieSliceFill(d.name, i)} />
                    ))}
                  </Pie>
                  <Tooltip
                    contentStyle={{
                      borderRadius: 12,
                      border: "1px solid var(--silv-border)",
                      boxShadow: "var(--silv-card-shadow)",
                    }}
                    formatter={(value) => {
                      const n = typeof value === "number" ? value : Number(value);
                      const frac = Number.isFinite(n) ? n : 0;
                      return [`${(frac * 100).toFixed(2)}%`, "Share"];
                    }}
                  />
                  <Legend
                    verticalAlign="bottom"
                    height={36}
                    formatter={(value) => <span className="portfolio-charts__legend">{value}</span>}
                  />
                </PieChart>
              </ResponsiveContainer>
            </div>
          </div>
        ) : null}

        {barData.length > 0 ? (
          <div className="silv-chart-card silv-card silv-card--venue portfolio-charts__bar-card">
            <h3 className="portfolio-charts__subtitle">Target vs actual</h3>
            <p className="portfolio-charts__hint muted">Weights as % of portfolio value</p>
            <ResponsiveContainer width="100%" height={340}>
              <BarChart
                data={barData}
                margin={{ top: 12, right: 12, left: 4, bottom: 8 }}
                barGap={0}
                barCategoryGap="18%"
              >
                <CartesianGrid strokeDasharray="3 3" stroke="rgba(16,23,32,0.08)" vertical={false} />
                <XAxis
                  dataKey="name"
                  tick={{ fill: "var(--silv-muted)", fontSize: 12 }}
                  axisLine={{ stroke: "var(--silv-border)" }}
                />
                <YAxis
                  tick={{ fill: "var(--silv-muted)", fontSize: 12 }}
                  axisLine={false}
                  tickLine={false}
                  domain={[0, "auto"]}
                  tickFormatter={(v) => `${v}%`}
                />
                <Tooltip
                  contentStyle={{
                    borderRadius: 12,
                    border: "1px solid var(--silv-border)",
                    boxShadow: "var(--silv-card-shadow)",
                  }}
                  formatter={(v) => {
                    const n = typeof v === "number" ? v : Number(v);
                    return [`${(Number.isFinite(n) ? n : 0).toFixed(2)}%`, ""];
                  }}
                />
                <Legend
                  formatter={(value) => (
                    <span style={{ color: "var(--silv-text, #111)" }}>{value}</span>
                  )}
                />
                <Bar dataKey="target" name="Target" fill={BAR_TARGET_SALAD} radius={[4, 0, 0, 0]}>
                  {barData.map((_, i) => (
                    <Cell key={`tgt-${i}`} fill={BAR_TARGET_SALAD} />
                  ))}
                </Bar>
                <Bar dataKey="actual" name="Actual" radius={[0, 4, 0, 0]}>
                  {barData.map((d, i) => (
                    <Cell key={`act-${d.name}-${i}`} fill={actualBarFill(d.name, i)} />
                  ))}
                </Bar>
              </BarChart>
            </ResponsiveContainer>
          </div>
        ) : null}
      </div>
    </section>
  );
}
