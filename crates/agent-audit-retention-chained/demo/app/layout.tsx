import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-audit-retention-chained demo",
  description: "Live-simulate the Silvana audit-retention agent: chained per-day/week log rotation.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
