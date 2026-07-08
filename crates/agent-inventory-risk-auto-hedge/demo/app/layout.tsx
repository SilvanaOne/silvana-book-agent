import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-inventory-risk-auto-hedge demo",
  description: "Live-simulate the Silvana inventory-risk agent: soft/hard band inventory signal + optional auto-hedge.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
