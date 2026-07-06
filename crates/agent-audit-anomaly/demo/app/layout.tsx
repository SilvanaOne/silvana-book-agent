import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-audit-anomaly demo",
  description: "Live-simulate the Silvana audit-anomaly agent: post-hoc statistical scan over trading-history.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
