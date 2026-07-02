# syntax=docker/dockerfile:1.6
# ---------------------------------------------------------------------------
# agent-spread-capture demo — production Docker image
#
# Build:   docker build -t agent-spread-capture-demo crates/agent-spread-capture/demo
# Run:     docker run --rm -p 3008:3004 --name spreadcapture-demo agent-spread-capture-demo
# Open:    http://<host>:3004
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
ENV PORT=3008
COPY --from=build /app/.next        ./.next
COPY --from=build /app/public       ./public
COPY --from=build /app/node_modules ./node_modules
COPY --from=build /app/package.json ./package.json
RUN addgroup --system --gid 1001 nodejs \
 && adduser  --system --uid 1001 nextjs \
 && chown -R nextjs:nodejs /app
USER nextjs
EXPOSE 3008
CMD ["npm", "run", "start"]
