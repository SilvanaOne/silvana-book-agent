import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-audit-attestation demo",
  description: "Live-simulate the Silvana audit-attestation agent: signed chain-head checkpoints for a trading-history log.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
