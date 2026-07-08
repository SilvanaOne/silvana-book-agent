import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-test-run-rpc-smoke demo",
  description: "Live-simulate the Silvana test-run agent: pre-flight healthcheck of every read-only gRPC that trading agents depend on.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
