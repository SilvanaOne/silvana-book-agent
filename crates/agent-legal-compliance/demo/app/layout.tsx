import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-legal-compliance demo",
  description: "Live-simulate the Silvana legal-compliance agent: jurisdiction rule evaluator.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
