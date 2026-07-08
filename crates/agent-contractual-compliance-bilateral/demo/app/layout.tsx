import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-contractual-compliance-bilateral demo",
  description: "Live-simulate the Silvana contractual-compliance agent: bilateral obligations against live settlement flow.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
