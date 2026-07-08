import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-audit-replay-back-test demo",
  description: "Live-simulate the Silvana audit-replay agent: offline policy back-test against a signed trading-history log.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
