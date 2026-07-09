import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-iceberg-execution-adaptive demo",
  description: "Live-simulate the Silvana Adaptive Iceberg agent: chunk size follows fill velocity on a synthetic price walk.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
