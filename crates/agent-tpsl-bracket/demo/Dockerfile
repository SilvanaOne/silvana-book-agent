# syntax=docker/dockerfile:1.6
# ---------------------------------------------------------------------------
# agent-tpsl-bracket demo — production Docker image
#
# Build:   docker build -t agent-tpsl-bracket-demo crates/agent-tpsl-bracket/demo
# Run:     docker run --rm -p 3076:3076 --name tpsl-demo agent-tpsl-bracket-demo
# Open:    http://<host>:3076
# ---------------------------------------------------------------------------

# 1) Install deps only (cached separately from source for fast rebuilds).
FROM node:20-alpine AS deps
WORKDIR /app
COPY package.json package-lock.json* ./
RUN npm ci --no-audit --no-fund

# 2) Build the Next.js production bundle.
FROM node:20-alpine AS build
WORKDIR /app
ENV NEXT_TELEMETRY_DISABLED=1
COPY --from=deps /app/node_modules ./node_modules
COPY . .
RUN npm run build

# 3) Minimal runtime image.
FROM node:20-alpine AS runtime
WORKDIR /app
ENV NODE_ENV=production
ENV NEXT_TELEMETRY_DISABLED=1
ENV PORT=3076
# Copy only what's needed to serve production.
COPY --from=build /app/.next        ./.next
COPY --from=build /app/public       ./public
COPY --from=build /app/node_modules ./node_modules
COPY --from=build /app/package.json ./package.json
# Drop root privileges.
RUN addgroup --system --gid 1001 nodejs \
 && adduser  --system --uid 1001 nextjs \
 && chown -R nextjs:nodejs /app
USER nextjs
EXPOSE 3076
# `npm run start` respects PORT via next.config.js (-p 3076 in package.json).
CMD ["npm", "run", "start"]
