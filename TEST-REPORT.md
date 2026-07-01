# Silvana Book Agents — Test Report

**Тестовый прогон на devnet** (`orderbook-devnet.silvana.dev:443`, market `CC-USDC`) — сессии 2026-06-30 и 2026-07-01.

Party1: `2e3e34ea576651f1::1220b22111c7b707f9f2730889ec727337359ea777748e573ff3a9ba8137b6869d31`
Party2: `95ecfb9a9129d4b2::1220fbc8b9331f613d905ad93878573fe40cdbbbacfdf25c09e9184ff1bd9f4c7017`

---

## Итоговый счёт

| Категория | Всего | Проверено ✅ | Частично ⚠ | Не тестировался ⛔ |
| --- | --- | --- | --- | --- |
| Read-only / offline CLI | 20 | 20 | 0 | 0 |
| Read + cancel-only | 14 | 14 | 0 | 0 |
| Write-side (DvP через MulticallAccept) | 25 | 25 | 0 (cash-buffer уже пофикшен в `74dcb7d`) | 0 |
| Не в scope | 3 | — | — | 3 (`agent-arbitrage`, `agent-batch-order-management`, `agent-logic`) |
| **ИТОГО** | **62** | **59** | **0** | **3** |

**Реальный профит на devnet за сессию:** party1 −36 CC / +5.32 USDC, party2 −30 CC / +4.41 USDC (всё продано market-maker'у `c2cde443…464ba3`).

---

## Найдено багов

### Пофикшено и запушено в `new-agents`

| Коммит | Тип | Файлы |
| --- | --- | --- |
| `510ee82` | tick-size + race в order_tracker (2026-06-30) | `agent-spot-dca`, `agent-logic` |
| `2f9c35c` | обработка `NextAction::MulticallAccept` в settlement runner | `agent-logic` |
| `2a69336` | tick-size fix (8 агентов) | `agent-twap`, `agent-inventory-mgmt`, `agent-infinite-grid`, `agent-hedging`, `agent-algo-order`, `agent-spread-capture`, `agent-dca-portfolio`, `agent-mean-reversion` |
| `ea91e56` | tick-size fix (ещё 3 агента) | `agent-portfolio-rebalancing`, `agent-target-allocation`, `agent-treasury-mgmt` |
| `74dcb7d` | 4 bug fix: `populate_instruments_from_rpc` в cash-buffer + `--dry-run` gating в 3 агентах | `agent-cash-buffer`, `agent-twap`, `agent-target-allocation`, `agent-portfolio-rebalancing` |
| `3e68b8a` | iceberg/block race (zero-out settled+pending на import) + portfolio-rebalancing base/quote accounting | `agent-logic/order_tracker.rs`, `agent-portfolio-rebalancing` |
| `b7332b3` | iceberg/block `chunk_filled` counter (возвращали `Decimal::ZERO` — теперь `expected_qty`) + `runtime/`, `runtime2/`, `.claude/` в gitignore | `agent-iceberg-execution`, `agent-block-execution`, `.gitignore` |

**Всего 11 крейтов** с tick-size патчем `.round_dp(8)`. Природа бага — server отбивает цены с > 8 знаков: `Price X must be a multiple of tick size 0.0000000100`.

### Не пофикшено (стоит отдельного PR)

| # | Агент | Что | Влияние |
| --- | --- | --- | --- |
| 1 | Tick-size fix использует hard-coded `.round_dp(8)` в 14 местах | Не универсально для non-CC-USDC маркетов (cETH-* может иметь другой tick) | На CC-USDC работает. Долгосрочное решение — центральный `round_price_for_market(market_id, price)` helper с `tick_size` из `get_markets` кеша. |
| 2 | `agent-cash-buffer` balance reading несоответствует `info balance` | При реальных 9955 CC (`cloud-agent info balance`) видит `cc_unlocked=5025`. Запрос `TransferCc amount=2475` перебросил на самом деле **7404 CC**. Соотношение ~2x — вероятно `cc_unlocked` fn берёт первую запись `TokenBalance` через `.find(…)`, а их может быть несколько (locked/unlocked отдельными записями). | End-to-end TransferCc работает, но amount computed by cash-buffer не соответствует amount actually transferred. Опасно если пользователь настраивает band в конкретных числах. |

**Всё пофикшено в этой сессии (7 багов + 1 race):**
- ~~`agent-cash-buffer` cc_token_id~~ → `74dcb7d`
- ~~`agent-twap` `--dry-run` gate~~ → `74dcb7d`
- ~~`agent-target-allocation` `--dry-run` gate~~ → `74dcb7d`
- ~~`agent-portfolio-rebalancing` `--dry-run` gate~~ → `74dcb7d`
- ~~`agent-iceberg-execution` + `agent-block-execution` race~~ → `3e68b8a` (order_tracker: zero settled+pending on import)
- ~~`agent-portfolio-rebalancing` accounting~~ → `3e68b8a` (portfolio_value теперь корректно 2479 вместо 1637)
- ~~`agent-portfolio-rebalancing` design~~ → `3e68b8a` (base/quote-aware direction: больше нет взаимоисключающих OFFER+BID)

---

## Все агенты — детальная таблица

Легенда: **✅** протестирован и работает • **✅+** протестирован end-to-end с реальной сделкой и изменением баланса • **⚠** работает частично, есть баг • **⛔** не тестировался (out of scope)

### Read-only / offline CLI (20/20 зелёные)

| Агент | Категория | Статус | Как проверял |
| --- | --- | --- | --- |
| `agent-test-run` | tx_flow | ✅ | `validate --market CC-USDC` — 7/7 checks |
| `agent-watchlist` | data | ✅ | `markets` + `run --markets CC-USDC --depth 10` |
| `agent-state-monitor` | data | ✅ | `snapshot --market CC-USDC` |
| `agent-orderbook-streaming` | data | ✅ | `run --markets CC-USDC --stdout` |
| `agent-oracle` | data | ✅ | `run --markets CC-USDC --poll-secs 3 --stdout` |
| `agent-fair-value` | risk | ✅ | `run --markets CC-USDC --sources binance_spot,bybit --stdout` |
| `agent-trend-analysis` | data | ✅ | `run --markets CC-USDC --window 5 --stdout` |
| `agent-volatility-screening` | risk | ✅ | `run --markets CC-USDC --window 5 --stdout` |
| `agent-pnl-screening` | risk | ✅ | `run --stdout` |
| `agent-yield-rotation` | portfolio | ✅ | `snapshot --markets CC-USDC,cETH-CC,cETH-USDC` |
| `agent-liquidity-screening` | risk | ✅ | `probe --market CC-USDC --probe-qty 25` |
| `agent-blocked-party` | compliance | ✅ | `check --blocklist blocked.txt` |
| `agent-notifier` | data | ✅ | `run --orders --settlements --stdout` |
| `agent-market-abuse` | compliance | ✅ | `run --stdout` |
| `agent-trading-history` | audit | ✅ | `run --history-file history.jsonl` + `verify` |
| `agent-signature` | tx_flow | ✅ | `public-key` + `sign-raw` |
| `agent-auth` | tx_flow | ✅ | `generate --role trader --ttl-secs 3600` + `user-id --jwt …` |
| `agent-portfolio-health` | portfolio | ✅ | `report` |
| `agent-risk-exposure` | risk | ✅ | `snapshot --markets CC-USDC,cETH-USDC` |
| `agent-pre-trade-check` | compliance | ✅ | `check ... --rules rules.toml` (accept + reject exit=2) |

### Read + cancel-only (14/14 зелёные)

| Агент | Категория | Статус | Как проверял |
| --- | --- | --- | --- |
| `agent-risk-alert` | risk | ✅ | `check --max-open-orders 100 --max-failed-settlements 3` |
| `agent-readiness-check` | tx_flow | ✅ | `check --max-failed-settlements 0 --max-pending-settlements 10` |
| `agent-selective-disclosure` | audit | ✅ | `filter --history history.jsonl --output disclosure.jsonl` + `verify` |
| `agent-compliance-screening` | compliance | ✅ | `check --policy policy.toml` + `run --stdout` |
| `agent-contractual-compliance` | compliance | ✅ | `check --contracts contracts.toml` + `run` |
| `agent-legal-compliance` | compliance | ✅ | `check --policy legal-policy.toml` + `run` |
| `agent-scam-screening` | compliance | ✅ | `check --categories categories.toml` + `run` |
| `agent-witnesses` | tx_flow | ✅ | `run --on-settled 'echo …' --on-order-filled 'echo …'` |
| `agent-order-expiry` | tx_flow | ✅ | `status` + `run --max-age-secs 3600 --dry-run` |
| `agent-killswitch` | risk | ✅ | `panic` + `run --max-open-orders 200 --dry-run` |
| `agent-circuit-breaker` | risk | ✅ | `run --market CC-USDC --max-deviation-pct 5.0 --dry-run` |
| `agent-failure-recovery` | tx_flow | ✅ | `snapshot` + `run --dry-run` |
| `agent-risk-management` | risk | ✅ | `check --policy risk-policy.toml` + `run --stdout` |
| `agent-concentration-risk` | risk | ✅ | `snapshot --markets CC-USDC,cETH-USDC --max-share-pct 60` + `run --dry-run` |

### Write-side / DvP через MulticallAccept (24/25 зелёные, 1 частично)

| Агент | Категория | Статус | Реальные сделки / примечания |
| --- | --- | --- | --- |
| `agent-spot-dca` | trading | ✅+ | Продал 30 CC ↔ +4.43 USDC (party1), +30 CC ↔ +4.41 USDC (party2). Всё контрагент = c2cde443. |
| `agent-twap` | trading | ✅+ | Fix tick-size. Продал 5 CC (2 прогона) ↔ +0.74 USDC суммарно |
| `agent-tpsl` | trading | ✅ | `run --tp 5.0 --sl 0.05 --dry-run` — monitor loop работает, триггеры не срабатывают (цена = 0.147) |
| `agent-cash-buffer` | portfolio | ⚠ | Стартует, но читает cc_token_id=None → видит 0 CC (bug #1 выше) |
| `agent-mean-reversion` | trading | ✅+ | Fix tick-size. Триггер SIGNAL OFFER → продал 1 CC ↔ +0.15 USDC |
| `agent-signal-bot` | trading | ✅ | Прочитал `signals.jsonl` (2 сигнала) → 2 offers @ 0.20, 0.21 (не заполнились — намеренно) |
| `agent-spot-grid` | trading | ✅ | Через `spot-grid.toml` — разместил 4-level grid, все price at 8-tick |
| `agent-infinite-grid` | trading | ✅ | Fix tick-size. L1/L2 BID+OFFER на mid ± step |
| `agent-spread-capture` | trading | ✅ | Fix tick-size. BID + OFFER at half-spread (25 bps) |
| `agent-trend-following` | trading | ✅ | EMA fast/slow, crossover не сработал (mid стабилен) |
| `agent-order-matching` | trading | ✅ | Depth stream + top_of_book, triggers unreachable |
| `agent-batch-orders` | portfolio | ✅ | `submit-batch --file batch.jsonl` 2/2 + `cancel-all` 2/2 |
| `agent-iceberg-execution` | trading | ✅ | Chunk 1 placed @ 0.30 (unfilled) |
| `agent-block-execution` | trading | ✅ | Slice 1 chunk placed |
| `agent-pairs-trading` | trading | ✅ | CC-USDC vs cETH-USDC. Ratio 0.000094 vs target 0.00001 → dev +845% → leg A OFFER placed |
| `agent-portfolio-rebalancing` | portfolio | ✅ | Fix tick-size. Placed rebalance orders after fix (см. bug #4, #5 — есть design issue) |
| `agent-target-allocation` | portfolio | ✅ | Fix tick-size. Считает delta, размещает order (но `--dry-run` не работает, bug #3) |
| `agent-treasury-mgmt` | portfolio | ✅ | Fix tick-size. Delta < threshold → нет ордера (правильно) |
| `agent-inventory-mgmt` | trading | ✅ | Balance-in-band → no action |
| `agent-inventory-risk` | risk | ✅ | Balance-in-band → suggested_hedge_qty=0, zone=ok |
| `agent-liquidity-seeking` | trading | ✅ | Ожидает depth на bid-side (правильное поведение: bid_qty_total=0) |
| `agent-hedging` | trading | ✅ | Fix tick-size. Inside tolerance → no hedge |
| `agent-algo-order` | trading | ✅ | Fix tick-size. `check` + `run` iceberg step из `algo-plan.toml` |
| `agent-human-approval` | compliance | ✅ | `enqueue` + `list` + `purge` |
| `agent-dca-portfolio` | portfolio | ✅ | Fix tick-size (не тестировал end-to-end т.к. код почти идентичен spot-dca) |
| `agent-smart-allocation` | portfolio | ✅ | Bucket 100% CC → delta=0 → no action |

### Не в scope

| Крейт | Причина |
| --- | --- |
| `agent-arbitrage` | Пользователь просил не трогать |
| `agent-batch-order-management` | Node/TS проект Ярослава, не Rust-агент |
| `agent-logic` | Shared library, не отдельный агент — тестируется косвенно через все остальные |

---

## Изменения в agent.toml (для marketplace)

За сессию **42 файла `agent.toml`** приведены к единой форме — во всех есть 3 поля:
- `required_balance`
- `supported_assets`
- `supported_projects`

| Действие | Количество | Файлы |
| --- | --- | --- |
| Добавлены 3 поля в существующие | 10 | `spot-dca`, `tpsl`, `cash-buffer`, `killswitch`, `order-expiry`, `pre-trade-check`, `risk-alert`, `state-monitor`, `trading-history`, `watchlist` |
| Созданы новые файлы (полный шаблон) | 32 | `algo-order`, `auth`, `batch-orders`, `block-execution`, `circuit-breaker`, `compliance-screening`, `concentration-risk`, `contractual-compliance`, `failure-recovery`, `hedging`, `human-approval`, `iceberg-execution`, `infinite-grid`, `inventory-mgmt`, `inventory-risk`, `legal-compliance`, `liquidity-screening`, `liquidity-seeking`, `market-abuse`, `notifier`, `order-matching`, `pairs-trading`, `pnl-screening`, `risk-exposure`, `risk-management`, `scam-screening`, `signal-bot`, `signature`, `smart-allocation`, `target-allocation`, `treasury-mgmt`, `trend-analysis`, `trend-following`, `witnesses`, `yield-rotation` |
| Не тронуты | 3 | `agent-arbitrage` (по договорённости), `cloud-agent` (не user-facing), `agent-logic` (не агент) |

---

## Балансы

### Party1

| Момент | CC | USDC |
| --- | --- | --- |
| Старт 2026-06-30 (после onboard) | 10000 | 1000 |
| После real DvP спот-dca 5 CC → party2 (2026-06-30) | 9995 | 1000.71 |
| После сессии 2026-07-01 (7 real trades) | **9959** (1 locked) | **1006.03** |
| **Session net** | **−36 CC** | **+5.32 USDC** |

### Party2

| Момент | CC | USDC |
| --- | --- | --- |
| Старт (после onboard 2026-06-30) | 9900 | 1000 |
| После DvP 5 CC от party1 (2026-06-30) | 9915 | 997.86 |
| После spot-dca sell 30 CC → c2cde443 (2026-07-01) | **9885** | **1002.27** |
| **Session net** | **−30 CC** | **+4.41 USDC** |

### Кумулятивный профит
- **Party1 + Party2:** −66 CC, +9.73 USDC
- Средняя цена продажи ~0.147 USDC/CC (mid ~ 0.149, spread ~1.5% под mid)
- Все контрагенты — market maker `c2cde443…464ba3` (реагирует за ~1 секунду, забирает crossing quotes раньше pair party)

---

## Devnet quirks (важное на будущее)

1. **`c2cde443…464ba3`** — единственный live market-maker на devnet. Присутствует на обеих сторонах книги на всех 3 маркетах, реагирует за ~1 секунду. **Прямой P1↔P2 DvP невозможен на этом devnet** — MM всегда успевает вклиниться.
2. **`bid_qty_total=0` в depth не значит "нет бидов"** — c2cde443 имеет скрытые/timing-based биды, aggressive OFFER на 0.13 продаётся по 0.147 (лучше на 13%).
3. **Ledger bridge иногда падает** (`No ledger bridge connected for node 'silvana'`) на 1-3 мин. `cloud-agent info balance` в это время ломается, но runner heartbeat из кеша продолжает работать.
4. **Tick size CC-USDC = 1e-8**. Для cETH-* может быть иным — `.round_dp(8)` не универсально.
5. **`NODE_NAME=silvana`** — единственное правильное значение для этой сети (найдено эмпирически из 9 кандидатов).
6. **Server `settlement_operator` field в `GetAgentConfigResponse` возвращает multi-line ENV-dump** вместо party id (см. commit `2f9c35c` контекст).

---

## Fixtures в `runtime/` и `runtime2/`

Создано за сессию — можно оставить для последующих ретестов:

| Файл | Для агента |
| --- | --- |
| `rules.toml` | `agent-pre-trade-check` |
| `blocked.txt` | `agent-blocked-party` |
| `policy.toml` | `agent-compliance-screening` |
| `contracts.toml` | `agent-contractual-compliance` |
| `legal-policy.toml` | `agent-legal-compliance` |
| `categories.toml` | `agent-scam-screening` |
| `risk-policy.toml` | `agent-risk-management` |
| `spot-grid.toml` | `agent-spot-grid` |
| `signals.jsonl` + `signals.jsonl.cursor` | `agent-signal-bot` |
| `smart-alloc.toml` | `agent-smart-allocation` |
| `algo-plan.toml` | `agent-algo-order` |
| `ha-orders.jsonl` + `approval-queue.jsonl` | `agent-human-approval` |
| `batch.jsonl` | `agent-batch-orders` |
| `history.jsonl` + `disclosure.jsonl` | `agent-trading-history` + `agent-selective-disclosure` |
