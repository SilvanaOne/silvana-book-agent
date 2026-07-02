import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-pairs-trading demo",
  description: "Live-simulate the Silvana Pairs Trading agent: EMA-based snap-back trading on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
