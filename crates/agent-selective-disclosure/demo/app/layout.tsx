import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-selective-disclosure demo",
  description: "Live-simulate the Silvana selective-disclosure agent: filter + re-sign a trading-history audit log.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
