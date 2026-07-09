import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "agent-order-expiry-price-drift demo",
  description: "Live-simulate the Silvana Order Expiry (Price Drift) agent: cancel own orders whose resting price has drifted too far from the current mid.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
