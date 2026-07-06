import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-anomaly-classifier demo",
  description: "Live-simulate the Silvana anomaly-classifier agent: cluster anomaly stream into spoofing / wash_trading / stuck / normal.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
