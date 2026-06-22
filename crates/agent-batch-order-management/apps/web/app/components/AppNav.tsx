"use client";

import Image from "next/image";
import Link from "next/link";
import { usePathname } from "next/navigation";

const ROUTES = [
  { href: "/", label: "Home" },
  { href: "/portfolio", label: "Portfolio" },
  { href: "/rebalance", label: "Rebalance" },
  { href: "/monitor", label: "Monitor" },
  { href: "/venues", label: "Venues" },
  { href: "/audit", label: "Audit" },
] as const;

const GITHUB_URL =
  "https://github.com/SilvanaOne/silvana-book-agent/tree/new-agents/crates/agent-batch-order-management";
const GUIDE_URL =
  "https://github.com/SilvanaOne/silvana-book-agent/blob/new-agents/crates/agent-batch-order-management/GUIDELINES.md";

export function AppNav() {
  const pathname = usePathname();

  return (
    <nav className="silv-nav">
      <div className="silv-nav__main-frame">
        <div className="silv-nav__cluster">
          <div className="silv-nav__brand">
            Batch <span className="silv-nav__brand-accent">Ops</span>
            <span className="silv-nav__badge" aria-hidden>
              AGENT
            </span>
          </div>

          <div className="silv-nav__links">
            {ROUTES.map(({ href, label }) => {
              const active = pathname === href || (href !== "/" && pathname.startsWith(`${href}/`));
              return (
                <Link
                  key={href}
                  href={href}
                  prefetch
                  className={`silv-nav__link${active ? " silv-nav__link--active" : ""}`}
                >
                  {label}
                </Link>
              );
            })}
          </div>
        </div>

        <div className="silv-nav__external-links">
          <a
            className="silv-nav__link"
            href={GITHUB_URL}
            target="_blank"
            rel="noopener noreferrer"
          >
            GitHub
          </a>
          <a
            className="silv-nav__link"
            href={GUIDE_URL}
            target="_blank"
            rel="noopener noreferrer"
          >
            Guide
          </a>
        </div>
      </div>

      <a
        className="silv-nav__silvana-mark"
        href="https://silvana.one"
        target="_blank"
        rel="noopener noreferrer"
        aria-label="Silvana — open website"
      >
        <Image src="/silvana-logo.svg" alt="Silvana" width={162} height={33} priority />
      </a>
    </nav>
  );
}
