import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-arbitrage demo",
  description:
    "Live-simulate the Silvana Arbitrage agent: a cross-venue spread scanner surfacing buy-low / sell-high opportunities across Canton venues.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
