import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-smart-allocation-buckets demo",
  description: "Live-simulate the Silvana smart-allocation agent: two-level bucket portfolio allocator with automatic rebalancing.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
