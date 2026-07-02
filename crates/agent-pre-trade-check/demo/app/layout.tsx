import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-pre-trade-check demo",
  description: "Live-simulate the Silvana Pre-Trade Check agent: EMA-based snap-back trading on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
