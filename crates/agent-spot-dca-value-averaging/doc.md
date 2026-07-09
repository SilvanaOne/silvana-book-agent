# Spot DCA — Value Averaging

> Accumulate assets gradually at a lower average cost by placing periodic spot orders on a fixed schedule.

## What it does

Value-averaging variant of Spot DCA. Instead of buying a *fixed base amount* every period, the loop targets a *linear cumulative notional* schedule: after `k` cycles the target is `value_per_period × k` quote-currency units. Each cycle it places whatever order is required to close the current gap between target and actually-placed notional. Effect: buys more base when price is low, less when price is high — an accumulation curve that's smoother in *value* terms than plain DCA.

## How it works

Every `--interval` seconds:
1. Increment `cycle` by 1.
2. `target_cumulative = value_per_period × cycle`.
3. If `target_cumulative > max_total_quote` (when set) — stop.
4. Fetch mid, apply `--price-offset-pct`, round to tick.
5. `gap = target_cumulative − placed_notional_so_far`. If `max_order_quote` is set, `gap = min(gap, max_order_quote)`.
6. `quantity = gap / order_price`. Sign and submit as BID (buy) or OFFER (sell).
7. On success, add the filled gap to `placed_notional_so_far`.

Notes:
- `placed_notional_so_far` tracks *submitted* notional, not settled fills — treat it as a target-tracking sensor, not a P&L view. Actual settlement is handled by the shared settlement backend as in every other Silvana agent.
- `--price-offset-pct` behaviour is identical to `Scheduled` DCA: negative for passive buys, positive for passive sells.

## Usage

```bash
agent-spot-dca-value-averaging run --market CC-USDC \
    --value-per-period 10 --interval 3600 --side buy

agent-spot-dca-value-averaging run --market CC-USDC \
    --value-per-period 25 --interval 1800 \
    --max-order-quote 100 --max-total-quote 1000

agent-spot-dca-value-averaging run --market CC-USDC \
    --value-per-period 50 --interval 3600 --price-offset-pct -0.5

agent-spot-dca-value-averaging status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `spot-dca` |
| Template | `Value Averaging` (rust) |
| Status | `active` |
| Tags | `dca`, `value-averaging`, `dynamic-size` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-spot-dca-value-averaging`
- Binary: `agent-spot-dca-value-averaging`

## Resources

- Live demo: <https://silvana.one/agents/demo/spot-dca-value-averaging/>
- Contact Us: <https://silvana.one/contact>
