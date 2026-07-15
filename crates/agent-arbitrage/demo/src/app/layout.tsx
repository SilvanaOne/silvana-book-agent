import type { Metadata } from 'next';
import { Inter, Sora, JetBrains_Mono } from 'next/font/google';
import './globals.css';
import { AuthProvider } from '@/lib/auth';
import { ThemeProvider, THEME_PREPAINT } from '@/lib/theme';
import { asset } from '@/lib/asset';

const sora = Sora({ subsets: ['latin'], weight: ['500', '600', '700'], variable: '--font-sora' });
const inter = Inter({ subsets: ['latin'], variable: '--font-inter' });
const jb = JetBrains_Mono({ subsets: ['latin'], variable: '--font-mono-jb' });

export const metadata: Metadata = {
  title: 'Arbitrage Agent',
  description: 'Canton-hosted arbitrage agent — operator dashboard',
  icons: {
    icon: [{ url: asset('/favicon.svg'), type: 'image/svg+xml' }],
    shortcut: [{ url: asset('/favicon.svg'), type: 'image/svg+xml' }],
  },
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    // suppressHydrationWarning: the prepaint script below mutates
    // <html data-theme> before React hydrates. Without this flag React would
    // (rightly) flag the attribute mismatch — but the mutation is intentional
    // and isolated to the <html> element, so we silence the warning here.
    <html
      lang="en"
      className={`${sora.variable} ${inter.variable} ${jb.variable}`}
      suppressHydrationWarning
    >
      <head>
        {/* Pre-paint the saved theme before hydration so neither variant flashes. */}
        <script dangerouslySetInnerHTML={{ __html: THEME_PREPAINT }} />
      </head>
      <body>
        <ThemeProvider>
          <AuthProvider>{children}</AuthProvider>
        </ThemeProvider>
      </body>
    </html>
  );
}
