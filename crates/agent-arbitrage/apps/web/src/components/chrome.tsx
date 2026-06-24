'use client';

import Link from 'next/link';
import { usePathname } from 'next/navigation';

export function Brand() {
  return (
    <span className="brand">
      <span className="brand-mark">A</span>
      Arbitrage<span className="accent-text">Agent</span>
      <span className="brand-tag">canton</span>
    </span>
  );
}

const TABS = [
  { href: '/', label: 'Dashboard' },
  { href: '/stats', label: 'Stats' },
  { href: '/config', label: 'Config' },
  { href: '/balances', label: 'Balances' },
  { href: '/assistant', label: 'AI Assistant' },
  { href: '/manager', label: 'AI Manager' },
] as const;

const GITHUB_URL =
  'https://github.com/SilvanaOne/silvana-book-agent/tree/new-agents/crates/agent-arbitrage';
const GUIDE_URL =
  'https://github.com/SilvanaOne/silvana-book-agent/blob/new-agents/crates/agent-arbitrage/GUIDELINES.md';

export function NavTabs() {
  const path = usePathname();
  return (
    <nav className="nav" aria-label="primary">
      {TABS.map((t) => (
        <Link key={t.href} href={t.href} data-active={path === t.href}>
          {t.label}
        </Link>
      ))}
      <span className="nav-ext-group">
        <a href={GITHUB_URL} target="_blank" rel="noopener noreferrer">
          GitHub
        </a>
        <a href={GUIDE_URL} target="_blank" rel="noopener noreferrer">
          Guide
        </a>
      </span>
    </nav>
  );
}
