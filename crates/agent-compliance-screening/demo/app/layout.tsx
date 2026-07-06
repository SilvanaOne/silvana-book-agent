import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-compliance-screening demo",
  description: "Live-simulate the Silvana compliance-screening agent: rule-engine for live settlement events.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
