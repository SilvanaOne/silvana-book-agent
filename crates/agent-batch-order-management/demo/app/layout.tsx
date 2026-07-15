import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-batch-order-management demo",
  description: "Target-weight portfolio rebalancing: NAV, per-asset drift, and a preview → execute → monitor rebalance job — all on in-memory fixtures.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
