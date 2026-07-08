import type { Metadata } from "next";
import "./globals.css";
export const metadata: Metadata = {
  title: "agent-news-sentiment-lexicon demo",
  description: "Live-simulate the Silvana news-sentiment agent: score headlines and emit market-tagged signals.",
};
export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (<html lang="en"><body>{children}</body></html>);
}
