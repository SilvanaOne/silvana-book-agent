import { fileURLToPath } from 'node:url';

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  // Served behind an nginx reverse proxy under this path prefix.
  basePath: '/agents/demo/arbitrage',
  // Exposed to client code so hardcoded public-asset paths (logo, venue glyphs,
  // favicon) can be prefixed — Next does NOT auto-prefix <img src>/CSS url().
  env: { NEXT_PUBLIC_BASE_PATH: '/agents/demo/arbitrage' },
  // The demo is self-contained; pin the tracing root to this directory so a
  // sibling lockfile higher in the monorepo isn't mistaken for the workspace root.
  outputFileTracingRoot: fileURLToPath(new URL('.', import.meta.url)),
};

export default nextConfig;
