import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-copy-trading-proportional demo",
  description: "Live-simulate the Silvana copy-trading agent: mirror a leader party's order flow onto your own book.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
