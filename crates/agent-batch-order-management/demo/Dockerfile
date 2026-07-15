# syntax=docker/dockerfile:1.6
# ---------------------------------------------------------------------------
# agent-batch-order-management demo — production Docker image
#
# Build:   docker build -t agent-batch-order-management-demo crates/agent-batch-order-management/demo
# Run:     docker run --rm -p 3089:3089 --name batch-order-management-demo agent-batch-order-management-demo
# Open:    http://<host>:3089
# ---------------------------------------------------------------------------

FROM node:20-alpine AS deps
WORKDIR /app
COPY package.json package-lock.json* ./
RUN npm ci --no-audit --no-fund

FROM node:20-alpine AS build
WORKDIR /app
ENV NEXT_TELEMETRY_DISABLED=1
COPY --from=deps /app/node_modules ./node_modules
COPY . .
RUN npm run build

FROM node:20-alpine AS runtime
WORKDIR /app
ENV NODE_ENV=production
ENV NEXT_TELEMETRY_DISABLED=1
ENV PORT=3089
COPY --from=build /app/.next        ./.next
COPY --from=build /app/public       ./public
COPY --from=build /app/node_modules ./node_modules
COPY --from=build /app/package.json ./package.json
# `next start` (non-standalone) reads next.config at RUNTIME — it carries basePath.
# Without this copy the server ignores basePath and serves from root.
COPY --from=build /app/next.config.ts ./next.config.ts
RUN addgroup --system --gid 1001 nodejs \
 && adduser  --system --uid 1001 nextjs \
 && chown -R nextjs:nodejs /app
USER nextjs
EXPOSE 3089
CMD ["npm", "run", "start"]
