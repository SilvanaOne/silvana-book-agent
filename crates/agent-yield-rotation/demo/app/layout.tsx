import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-yield-rotation demo",
  description: "Live-simulate the Silvana Yield Rotation agent: rank markets by carry score and emit rotation signals on top-market changes.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
