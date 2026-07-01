# syntax=docker/dockerfile:1.6
# ---------------------------------------------------------------------------
# agent-spot-dca demo — production Docker image
#
# Build:   docker build -t agent-spot-dca-demo crates/agent-spot-dca/demo
# Run:     docker run --rm -p 3003:3003 --name dca-demo agent-spot-dca-demo
# Open:    http://<host>:3003
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
ENV PORT=3003
COPY --from=build /app/.next        ./.next
COPY --from=build /app/public       ./public
COPY --from=build /app/node_modules ./node_modules
COPY --from=build /app/package.json ./package.json
RUN addgroup --system --gid 1001 nodejs \
 && adduser  --system --uid 1001 nextjs \
 && chown -R nextjs:nodejs /app
USER nextjs
EXPOSE 3003
CMD ["npm", "run", "start"]
