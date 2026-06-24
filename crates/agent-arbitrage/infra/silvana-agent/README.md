# Silvana cloud-agent sidecar

Placeholder directory for Sprint 3+ deliverable. Contains the `cloud-agent`
Rust sidecar binary, its runtime config, and onboarding scripts.

## Status (Sprint 0)

Empty placeholder. To populate:

1. **Sprint 3 task:** port the `cloud-agent` binary and onboarding scripts
   from the BAM project:

   ```bash
   cp -r /home/mdb1/business/web3soft/canton/batch_orderbook_management_agent/\
   batch-order-agent/infra/silvana-agent/{Dockerfile,prepare.sh,onboard.sh,smoke.sh} \
     infra/silvana-agent/
   ```

2. **Onboarding flow** (when Silvana credit_limit is unblocked):

   ```bash
   cd infra/silvana-agent
   ./prepare.sh                   # download cloud-agent binary from GitHub releases
   cp .env.example .env           # fill in INVITE_CODE, AGENT_NAME, EMAIL
   ./onboard.sh                   # generates keypair, registers party
   ./sync-to-env.sh               # copy PARTY_AGENT / private key into root .env
   ```

3. **Verify:**

   ```bash
   docker compose \
     -f infra/docker-compose.yml \
     -f infra/cloud-agent.compose.yml \
     up -d --build cloud-agent
   docker logs arb-cloud-agent
   ```

## Documentation

- `md_docs/14-silvana-host-runtime.md` — architecture
- `md_docs/09-development-roadmap.md` Sprint 3 — when this gets populated
- BAM reference: `batch_orderbook_management_agent/task/updated-scenario/RUNBOOK.md`
- Upstream: <https://github.com/SilvanaOne/silvana-book-agent>
