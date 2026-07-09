import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-tpsl-bracket demo",
  description: "Live-simulate the Silvana Take-Profit / Stop-Loss agent with a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
