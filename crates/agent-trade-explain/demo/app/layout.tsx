import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-trade-explain demo",
  description: "Live-simulate the Silvana trade-explain agent: post-hoc LLM rationale + counterfactual for every trade.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
