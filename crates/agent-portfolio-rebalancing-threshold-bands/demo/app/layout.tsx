import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-portfolio-rebalancing-threshold-bands demo",
  description: "Live-simulate the Silvana Portfolio Rebalancing agent (Threshold Bands variant): band-triggered single-shot rebalances to target weights.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
