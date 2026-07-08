import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-cash-buffer-push-only demo",
  description: "Live-simulate the Silvana Cash Buffer agent: EMA-based snap-back trading on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
