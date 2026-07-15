import { fileURLToPath } from 'node:url';

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  // Served behind an nginx reverse proxy under this path prefix.
  basePath: '/agents/demo/arbitrage',
  // The demo is self-contained; pin the tracing root to this directory so a
  // sibling lockfile higher in the monorepo isn't mistaken for the workspace root.
  outputFileTracingRoot: fileURLToPath(new URL('.', import.meta.url)),
};

export default nextConfig;
