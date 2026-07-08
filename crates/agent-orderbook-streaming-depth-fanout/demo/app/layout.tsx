import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-orderbook-streaming-depth-fanout demo",
  description: "Live-simulate the Silvana orderbook-streaming agent: fan out depth + external prices to JSONL sinks.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
