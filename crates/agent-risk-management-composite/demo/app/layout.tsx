import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-risk-management-composite demo",
  description: "Live-simulate the Silvana risk-management agent: composite policy limits, optional enforce-mode cancellations.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
