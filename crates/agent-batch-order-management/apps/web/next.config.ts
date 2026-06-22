import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  reactStrictMode: true,

  /** Минимальный runtime-образ в Docker (`deploy/Dockerfile.web`). */
  output: "standalone",
};

export default nextConfig;
