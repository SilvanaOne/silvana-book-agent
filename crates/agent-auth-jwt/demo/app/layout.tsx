import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-auth-jwt demo",
  description: "Live-simulate the Silvana Auth agent: JWT generation, decoding, verification, and rotation on a synthetic clock.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
