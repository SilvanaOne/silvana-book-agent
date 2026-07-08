import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-algo-order-dispatcher demo",
  description: "Live-simulate the Silvana algo-order agent: pluggable execution dispatcher (TWAP / Iceberg / Liquidity-Seeking).",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
