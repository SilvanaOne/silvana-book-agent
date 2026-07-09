import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-trend-following-macd demo",
  description: "Live-simulate the Silvana Trend Following Macd agent: MACD-crossover momentum trading on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
