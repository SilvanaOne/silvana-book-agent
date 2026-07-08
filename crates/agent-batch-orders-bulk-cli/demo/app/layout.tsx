import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-batch-orders-bulk-cli demo",
  description: "Live-simulate the Silvana Batch Orders agent: EMA-based snap-back trading on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
