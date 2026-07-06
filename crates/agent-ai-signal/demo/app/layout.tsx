import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-ai-signal demo",
  description: "Live-simulate the Silvana ai-signal agent: LLM turns a natural-language prompt into a structured trading signal.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
