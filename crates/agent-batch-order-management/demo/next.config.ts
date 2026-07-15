import path from "node:path";
import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  reactStrictMode: true,

  // Served behind an nginx reverse proxy under this path prefix.
  basePath: "/agents/demo/batch-order-management",

  /**
   * This demo lives inside the monorepo, which has other lockfiles. Pin the tracing root
   * to the demo dir so build-time file tracing is self-contained and warning-free.
   * (No `output: "standalone"` — the Docker image runs `npm run start` = `next start -p 3089`.)
   */
  outputFileTracingRoot: path.join(process.cwd()),

  /**
   * DEMO build: hardcode demo mode ON so the drift-imitator tool renders and no env is
   * required. `NEXT_PUBLIC_*` values are inlined at build time.
   */
  env: {
    NEXT_PUBLIC_DEMO_TOOLS: "1",
    NEXT_PUBLIC_BASE_PATH: "/agents/demo/batch-order-management",
  },
};

export default nextConfig;
