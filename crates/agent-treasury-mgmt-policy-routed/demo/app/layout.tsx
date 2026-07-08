import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-treasury-mgmt-policy-routed demo",
  description: "Live-simulate the Silvana treasury-mgmt agent: policy-driven rebalancer with approval routing.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
