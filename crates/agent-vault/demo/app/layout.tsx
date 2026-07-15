import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Silvana Vault",
  description:
    "Multi-tenant browser-encrypted secrets vault for Silvana agents — Canton wallets + Bybit / KuCoin API keys.",
};

const themeBootstrap = `try{var t=localStorage.getItem('sv.theme');if(t==='light'||t==='dark')document.documentElement.setAttribute('data-theme',t);else document.documentElement.setAttribute('data-theme','dark');}catch(e){document.documentElement.setAttribute('data-theme','dark');}`;

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <head>
        <link rel="preconnect" href="https://fonts.googleapis.com" />
        <link
          rel="preconnect"
          href="https://fonts.gstatic.com"
          crossOrigin=""
        />
        <link
          href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600&family=JetBrains+Mono:wght@400;600&family=Sora:wght@600;700&display=swap"
          rel="stylesheet"
        />
        <link rel="icon" href="/silvana-mark.svg" type="image/svg+xml" />
        <script dangerouslySetInnerHTML={{ __html: themeBootstrap }} />
      </head>
      <body>{children}</body>
    </html>
  );
}
