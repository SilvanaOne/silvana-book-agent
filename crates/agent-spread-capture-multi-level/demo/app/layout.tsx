import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-spread-capture-multi-level demo",
  description: "Live-simulate the Silvana Spread Capture (multi-level) agent: a laddered two-sided market maker on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
