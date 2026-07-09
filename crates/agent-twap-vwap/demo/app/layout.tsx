import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-twap-vwap demo",
  description: "Live-simulate the Silvana TWAP VWAP agent: volume-weighted slice sizing over a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
