import { createGrpcTransport } from "@connectrpc/connect-node";
import { OrderbookClient, PricingClient } from "@silvana-one/orderbook";
import { dirname, resolve } from "node:path";
import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

function loadEnvFile(filePath: string): void {
  if (!existsSync(filePath)) return;
  const raw = readFileSync(filePath, "utf8");
  for (const line of raw.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const eq = trimmed.indexOf("=");
    if (eq <= 0) continue;
    const key = trimmed.slice(0, eq).trim();
    let val = trimmed.slice(eq + 1).trim();
    if (
      (val.startsWith('"') && val.endsWith('"')) ||
      (val.startsWith("'") && val.endsWith("'"))
    ) {
      val = val.slice(1, -1);
    }
    if (!(key in process.env)) process.env[key] = val;
  }
}

// ../../.env = монорепо корень batch-order-agent/.env
loadEnvFile(resolve(__dirname, "../../.env"));

async function subscribeOrdersBrief(
  client: OrderbookClient,
  ms: number
): Promise<void> {
  const deadline = Date.now() + ms;
  try {
    let n = 0;
    const marketId =
      process.env.SMOKE_ORDERS_MARKET_ID ??
      process.env.DEFAULT_MARKET ??
      undefined;
    for await (const ev of client.subscribeOrders({ marketId })) {
      if (Date.now() > deadline) break;
      n += 1;
      console.log(
        "[subscribeOrders] event",
        JSON.stringify(ev, (_k, v) => (typeof v === "bigint" ? v.toString() : v))
      );
      if (n >= 5) break;
    }
    if (n === 0)
      console.log("[subscribeOrders] no events within timeout (OK if book quiet)");
  } catch (e) {
    console.error("[subscribeOrders] stream error:", e);
    throw e;
  }
}

async function main() {
  const baseUrl =
    process.env.SILVANA_RPC?.trim() ||
    process.env.ORDERBOOK_GRPC_URL?.trim() ||
    "https://orderbook-devnet.silvana.dev:443";

  console.log(`Using baseUrl=${baseUrl}`);

  const transport = createGrpcTransport({ baseUrl });

  const pricing = new PricingClient({ transport });
  const probeMarket =
    process.env.DEFAULT_MARKET?.trim() || "CC-USDC";

  console.log(`\n--- PricingClient.getPrice (no JWT) — market=${probeMarket}`);
  try {
    const px = await pricing.getPrice({ marketId: probeMarket });
    console.log(
      "getPrice OK:",
      JSON.stringify(px, (_k, v) => (typeof v === "bigint" ? v.toString() : v))
    );
  } catch (e) {
    console.error("getPrice failed:", e);
    console.error("If TLS or network fails, fix SILVANA_RPC / firewall first.");
  }

  const token = process.env.SILVANA_JWT?.trim();
  if (!token) {
    console.log("\nSILVANA_JWT not set — skipping OrderbookClient authenticated calls.");
    console.log(
      "After onboard, copy JWT into batch-order-agent/.env (task/docs/phase-1-onboarding-and-sdk.md)."
    );
    return;
  }

  const ob = new OrderbookClient({ transport, token });

  console.log("\n--- OrderbookClient.getMarkets");
  try {
    const markets = await ob.getMarkets();
    const list = markets.markets.slice(0, 15);
    console.log(
      `getMarkets OK (showing first ${Math.min(15, markets.markets.length)} of ${markets.markets.length})`
    );
    console.log(JSON.stringify(list, (_k, v) => (typeof v === "bigint" ? v.toString() : v), 2));
  } catch (e) {
    console.error("getMarkets failed (check JWT validity / party onboarding):", e);
    throw e;
  }

  const orderProbeMarket =
    process.env.DEFAULT_MARKET?.trim() || "CC-USDC";
  console.log(`\n--- OrderbookClient.getOrders — market=${orderProbeMarket}`);
  try {
    const bundle = await ob.getOrders({ marketId: orderProbeMarket });
    const dumped = JSON.stringify(bundle, (_k, v) =>
      typeof v === "bigint" ? v.toString() : v
    );
    console.log(
      "getOrders OK (truncated to 2500 chars)" +
        (dumped.length > 2500 ? "…\n" : "\n"),
      dumped.length > 2500 ? dumped.slice(0, 2500) : dumped
    );
  } catch (e) {
    console.error("getOrders failed:", e);
    throw e;
  }

  if (process.env.SMOKE_SUBSCRIBE_ORDERS === "1") {
    console.log(
      "\n--- OrderbookClient.subscribeOrders (SMOKE_SUBSCRIBE_ORDERS=1, ≤8s or 5 msgs)"
    );
    await subscribeOrdersBrief(ob, 8000);
  } else {
    console.log(
      "\n(skip subscribeOrders; set SMOKE_SUBSCRIBE_ORDERS=1 to test streaming — may wait for server events)"
    );
  }

  console.log("\nSmoke finished.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
