-- One isolated database per proprietary engine (defense in depth: each engine
-- owns its own DB, separate from open-core's `arbitrage_agent`). Run on first
-- init of the dev `engines-postgres` container (mock-engines-compose.yml).
CREATE DATABASE signer;
CREATE DATABASE executor;
CREATE DATABASE rebalancer;
