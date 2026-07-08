// Port of agent-orderbook-streaming-depth-fanout to TypeScript. Mirrors
// crates/agent-orderbook-streaming-depth-fanout/src/main.rs — subscribes to internal
// depth (SubscribeOrderbookDepth) + external prices (StreamPrices) per
// market and dispatches every update to configured sinks: stdout / file /
// webhook.
//
// The demo simulates both streams (depth snapshot + delta walk, price
// ticks with best bid/offer) and each sink's success/failure. Snapshots
// are emitted every SNAPSHOT_EVERY_TICKS and deltas every tick when the
// top-of-book moves.

export type SinkKind = "stdout" | "file" | "webhook";

export type Sink = {
  kind: SinkKind;
  label: string;
  target?: string;
  deliveredCount: number;
  failedCount: number;
};

export type ObStreamConfig = Readonly<{
  markets: string[];                // ["CC-USDC", "BTC-USD"]
  depth: number;                    // max levels per side (>= 1)
  includeOrderbook: boolean;        // attach ob summary to price events
  includeTrades: boolean;           // attach random trade tick to price events
  noDepth: boolean;                 // disable internal depth subscription
  noPrices: boolean;                // disable external price stream
  sinks: ReadonlyArray<Readonly<{ kind: SinkKind; label: string; target?: string }>>;
  startingPrices: number[];         // one per market (fallback = first entry)
  webhookFailureRate: number;       // 0..1 — probability a webhook POST fails
  source: PriceSource;              // upstream label attached to price events
}>;

export type PriceSource = "binance_spot" | "bybit" | "coingecko" | "silvana.oracle";

export type EventKind =
  | "price"
  | "depth.snapshot"
  | "depth.delta";

export type Level = Readonly<{ price: number; qty: number; orders: number }>;

export type MarketBook = {
  market: string;
  price: number;                    // walking mid
  bestBid: number;
  bestAsk: number;
  bids: Level[];                    // sorted desc by price, up to depth
  offers: Level[];                  // sorted asc by price, up to depth
  seq: number;
  priceCount: number;
  depthCount: number;
  lastEventKind: EventKind | null;
  lastEventAt: number | null;
};

export type EventItem = Readonly<{
  seq: number;
  t: number;
  market: string;
  kind: EventKind;
  payload: Record<string, unknown>;
}>;

export type ObStreamState = {
  config: ObStreamConfig;
  status: "streaming" | "idle";
  books: MarketBook[];
  totalEvents: number;
  totalDelivered: number;
  totalFailed: number;
  priceEvents: number;
  snapshotEvents: number;
  deltaEvents: number;
  eventsPerMinute: number;
  events: EventItem[];              // bounded ~80
  sinks: Sink[];                    // per-sink counters
  tickCount: number;
};

const MAX_EVENTS = 80;
const SNAPSHOT_EVERY_TICKS = 10;

export function initState(config: ObStreamConfig, now: number): ObStreamState {
  const books: MarketBook[] = config.markets.map((m, i) => {
    const seed = config.startingPrices[i] ?? config.startingPrices[0] ?? 1;
    return newBook(m, seed, config.depth);
  });
  const sinks: Sink[] = config.sinks.map((s) => ({ ...s, deliveredCount: 0, failedCount: 0 }));
  const _ = now;  // eslint-disable-line @typescript-eslint/no-unused-vars
  return {
    config,
    status: "streaming",
    books,
    totalEvents: 0,
    totalDelivered: 0,
    totalFailed: 0,
    priceEvents: 0,
    snapshotEvents: 0,
    deltaEvents: 0,
    eventsPerMinute: 0,
    events: [],
    sinks,
    tickCount: 0,
  };
}

function newBook(market: string, seed: number, depth: number): MarketBook {
  const spreadBps = 15 + Math.random() * 20; // 15..35 bps
  const bestBid = seed * (1 - spreadBps / 20000);
  const bestAsk = seed * (1 + spreadBps / 20000);
  return {
    market,
    price: seed,
    bestBid,
    bestAsk,
    bids: buildLadder(bestBid, "bid", depth),
    offers: buildLadder(bestAsk, "offer", depth),
    seq: 0,
    priceCount: 0,
    depthCount: 0,
    lastEventKind: null,
    lastEventAt: null,
  };
}

function buildLadder(topPrice: number, side: "bid" | "offer", depth: number): Level[] {
  const step = topPrice * 0.0005;   // 5 bps per level
  const out: Level[] = [];
  for (let i = 0; i < depth; i++) {
    const p = side === "bid" ? topPrice - i * step : topPrice + i * step;
    const qty = Math.round((5 + Math.random() * 45) * 100) / 100; // 5..50
    const orders = 1 + Math.floor(Math.random() * 5);
    out.push({ price: round8(p), qty, orders });
  }
  return out;
}

/** Applies one tick. Simulates upstream events + per-sink delivery. */
export function step(state: ObStreamState, price: number, now: number): { state: ObStreamState; log: string[] } {
  if (state.status !== "streaming" || price <= 0) return { state, log: [] };
  const log: string[] = [];
  state.tickCount += 1;

  // 1. Walk each market's mid independently around the driver price so all
  //    markets share correlation with the "master" GBM but diverge slightly.
  for (const book of state.books) {
    book.price = jitter(book.price === state.books[0].price ? price : book.price, 0.003);
    const spreadBps = 15 + Math.random() * 10;
    book.bestBid = round8(book.price * (1 - spreadBps / 20000));
    book.bestAsk = round8(book.price * (1 + spreadBps / 20000));
    book.bids = buildLadder(book.bestBid, "bid", state.config.depth);
    book.offers = buildLadder(book.bestAsk, "offer", state.config.depth);
  }

  // 2. Emit events for each market.
  const candidates: Array<{ market: string; kind: EventKind; payload: Record<string, unknown> }> = [];
  for (const book of state.books) {
    if (!state.config.noPrices) {
      const payload: Record<string, unknown> = {
        market_id: book.market,
        price: round8(book.price),
        bid: book.bestBid,
        ask: book.bestAsk,
        source: state.config.source,
      };
      if (state.config.includeOrderbook) {
        payload.orderbook = { bids: book.bids.length, asks: book.offers.length };
      }
      if (state.config.includeTrades && Math.random() < 0.35) {
        payload.trade = {
          price: round8(book.price + (Math.random() - 0.5) * book.price * 0.001),
          qty: Math.round(Math.random() * 500) / 100 + 0.01,
          buyer_maker: Math.random() < 0.5,
        };
      }
      candidates.push({ market: book.market, kind: "price", payload });
      book.priceCount += 1;
      state.priceEvents += 1;
    }

    if (!state.config.noDepth) {
      const isSnapshot = state.tickCount % SNAPSHOT_EVERY_TICKS === 1;
      book.seq += 1;
      const kind: EventKind = isSnapshot ? "depth.snapshot" : "depth.delta";
      const payload: Record<string, unknown> = {
        market_id: book.market,
        update_type: isSnapshot ? "snapshot" : "delta",
        sequence: book.seq,
        bids: isSnapshot ? book.bids : book.bids.slice(0, 3),
        offers: isSnapshot ? book.offers : book.offers.slice(0, 3),
      };
      candidates.push({ market: book.market, kind, payload });
      book.depthCount += 1;
      if (isSnapshot) state.snapshotEvents += 1;
      else state.deltaEvents += 1;
    }
  }

  // 3. Dispatch every candidate to every sink.
  for (const c of candidates) {
    const seq = state.totalEvents + 1;
    const item: EventItem = { seq, t: now, market: c.market, kind: c.kind, payload: c.payload };
    state.events.push(item);
    if (state.events.length > MAX_EVENTS) state.events.shift();
    state.totalEvents = seq;
    const book = state.books.find((b) => b.market === c.market);
    if (book) { book.lastEventKind = c.kind; book.lastEventAt = now; }

    for (const sink of state.sinks) {
      const willFail = sink.kind === "webhook" && Math.random() < state.config.webhookFailureRate;
      if (willFail) {
        sink.failedCount += 1;
        state.totalFailed += 1;
        log.push(`SINK-FAIL [${sink.label}] ${c.kind} ${c.market} — HTTP 5xx`);
      } else {
        sink.deliveredCount += 1;
        state.totalDelivered += 1;
        log.push(`DISPATCH [${sink.label}] ${c.kind} ${c.market} ${describe(c)}`);
      }
    }
  }

  // 4. Rolling events/min.
  const windowMs = 60_000;
  const cutoff = now - windowMs;
  state.eventsPerMinute = state.events.filter((e) => e.t >= cutoff).length;

  return { state, log };
}

function describe(c: { kind: EventKind; payload: Record<string, unknown> }): string {
  if (c.kind === "price") {
    const bid = c.payload.bid as number | undefined;
    const ask = c.payload.ask as number | undefined;
    return `bid=${fmt(bid)} ask=${fmt(ask)}`;
  }
  const seq = c.payload.sequence as number | undefined;
  return `seq=${seq ?? "?"}`;
}

function jitter(current: number, vol: number): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
  const factor = 1 + vol * z;
  const next = current * factor;
  return next > 0 ? next : current * 0.5;
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmt(n: number | undefined): string {
  if (n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
