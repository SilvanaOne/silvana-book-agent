import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-risk-alert demo",
  description: "Live-simulate the Silvana Risk Alert agent: passive threshold monitor that emits alerts when open orders, failed settlements, or open notional breach configured limits.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
