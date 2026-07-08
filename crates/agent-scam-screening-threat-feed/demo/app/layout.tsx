import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-scam-screening-threat-feed demo",
  description: "Live-simulate the Silvana scam-screening agent: categorized threat-feed screener.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
