import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-pairs-trading-zscore demo",
  description: "Live-simulate the Silvana Pairs Trading agent: rolling-window z-score stat-arb on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
