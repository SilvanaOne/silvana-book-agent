import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-strategy-genesis-keyword demo",
  description: "Live-simulate the Silvana strategy-genesis agent: compile natural-language execution specs into algo-order TOML plans.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
