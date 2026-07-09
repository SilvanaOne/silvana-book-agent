import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-volatility-screening-parkinson demo",
  description: "Live-simulate the Silvana Volatility agent: Parkinson range-based realized-volatility on a synthetic high/low bar stream.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
