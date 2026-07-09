import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-trend-analysis-multi-timeframe demo",
  description: "Live-simulate the Silvana Trend Analysis agent (Multi-Timeframe variant): three simultaneous SMAs and a confluence alignment signal on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
