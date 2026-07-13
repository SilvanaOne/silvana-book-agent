//! RFQ V2 AtomicDVP venue registry (design §5.6).
//!
//! Discovers the LP's on-ledger `AtomicDVP` venues (one per pair), validates
//! them against the local market config + quote key, and exposes the validated
//! market ids for the AtomicRfqStream handshake. Venue CREATION is CLI setup —
//! never the agent — and goes through `AtomicDVPService_CreateVenue` on the
//! provider-signed singleton (server-built; the ledger-service discloses the
//! service — fa-design G2). Any validation failure disables rfq_v2 for that
//! market only (logged loudly, never crashes); v1 keeps working.

use std::collections::HashMap;
use std::sync::Mutex;

use tracing::{info, warn};

use crate::ledger_client::DAppProviderClient;

pub const TEMPLATE_ATOMIC_DVP: &str = "#atomic-dvp-v2:AtomicDVP:AtomicDVP";
pub const TEMPLATE_SETTLEMENT_TICKET: &str = "#atomic-dvp-v2:AtomicDVP:SettlementTicket";
pub const TEMPLATE_ATOMIC_DVP_SERVICE: &str = "#atomic-dvp-v2:AtomicDVP:AtomicDVPService";

/// A validated (or not) on-ledger AtomicDVP venue owned by this LP.
#[derive(Debug, Clone)]
pub struct VenueEntry {
    pub contract_id: String,
    pub template_id: String,
    pub created_event_blob: String,
    /// create-arguments JSON (verbatim value; string-typed decimals preserved)
    pub payload: serde_json::Value,
    pub synchronizer_id: String,
    /// the global featured-app party — a venue co-signatory (fa-design G1/G4)
    pub provider: String,
    pub pair_name: String,
    pub base_admin: String,
    pub base_id: String,
    pub quote_admin: String,
    pub quote_id: String,
    pub quote_public_key: String,
}

/// What the local config expects a market's venue to look like
/// (from `config.resolve_instrument` on the market's base/quote instruments).
#[derive(Debug, Clone)]
pub struct ExpectedVenue {
    pub base_id: String,
    pub base_admin: String,
    pub quote_id: String,
    pub quote_admin: String,
}

pub struct VenueRegistry {
    party_id: String,
    /// Local quote key SPKI hex (compared case-insensitively to the on-ledger key)
    local_spki_hex: String,
    /// market_id -> expected instruments (rfq_v2-enabled markets only)
    expected: HashMap<String, ExpectedVenue>,
    /// pair_name (== market_id) -> discovered venue
    venues: Mutex<HashMap<String, VenueEntry>>,
}

impl VenueRegistry {
    pub fn new(
        party_id: String,
        local_spki_hex: String,
        expected: HashMap<String, ExpectedVenue>,
    ) -> Self {
        Self {
            party_id,
            local_spki_hex,
            expected,
            venues: Mutex::new(HashMap::new()),
        }
    }

    /// Refresh venues from the ACS: `AtomicDVP` contracts with `lp == party_id`,
    /// keyed by pairName. Called at startup and whenever the updates watcher
    /// sees an AtomicDVP create/archive (key rotation = archive+create).
    pub async fn refresh(&self, client: &mut DAppProviderClient) -> anyhow::Result<()> {
        let contracts = client
            .get_active_contracts(&[TEMPLATE_ATOMIC_DVP.to_string()])
            .await?;

        let mut found: HashMap<String, VenueEntry> = HashMap::new();
        for c in contracts {
            let Some(ref args) = c.create_arguments else { continue };
            let payload = crate::prost_struct_to_json(args);
            let s = |ptr: &str| payload.pointer(ptr).and_then(|v| v.as_str()).map(str::to_string);
            let (Some(lp), Some(pair_name)) = (s("/lp"), s("/pairName")) else {
                continue;
            };
            if lp != self.party_id {
                continue;
            }
            let entry = VenueEntry {
                contract_id: c.contract_id,
                template_id: c.template_id,
                created_event_blob: c.created_event_blob,
                synchronizer_id: c.synchronizer_id,
                provider: s("/provider").unwrap_or_default(),
                pair_name: pair_name.clone(),
                base_admin: s("/baseInstrumentId/admin").unwrap_or_default(),
                base_id: s("/baseInstrumentId/id").unwrap_or_default(),
                quote_admin: s("/quoteInstrumentId/admin").unwrap_or_default(),
                quote_id: s("/quoteInstrumentId/id").unwrap_or_default(),
                quote_public_key: s("/quotePublicKey").unwrap_or_default(),
                payload,
            };
            found.insert(pair_name, entry);
        }

        info!("Venue registry refreshed: {} venue(s) for {}", found.len(), self.party_id);
        *self.venues.lock().unwrap() = found;
        Ok(())
    }

    /// The venue for a market, only if it passes validation:
    /// instruments match the config-resolved expectation and the on-ledger
    /// quotePublicKey matches the local SPKI (case-insensitive).
    pub fn validated(&self, market_id: &str) -> Option<VenueEntry> {
        let expected = self.expected.get(market_id)?;
        let venues = self.venues.lock().unwrap();
        let venue = venues.get(market_id)?;
        if venue.base_id != expected.base_id
            || venue.base_admin != expected.base_admin
            || venue.quote_id != expected.quote_id
            || venue.quote_admin != expected.quote_admin
        {
            warn!(
                "Venue for {} fails instrument validation: on-ledger ({}/{}, {}/{}) != expected ({}/{}, {}/{})",
                market_id,
                venue.base_admin, venue.base_id, venue.quote_admin, venue.quote_id,
                expected.base_admin, expected.base_id, expected.quote_admin, expected.quote_id,
            );
            return None;
        }
        if venue.created_event_blob.is_empty() {
            warn!("Venue for {} has no created_event_blob — not disclosable", market_id);
            return None;
        }
        if !venue.quote_public_key.eq_ignore_ascii_case(&self.local_spki_hex) {
            warn!(
                "Venue for {} quotePublicKey does not match the local quote key — \
                 key rotated or wrong keyfile; rfq_v2 disabled for this market",
                market_id
            );
            return None;
        }
        Some(venue.clone())
    }

    /// Markets (from the expected/rfq_v2-enabled set) whose venue validates —
    /// the handshake `market_ids`.
    pub fn validated_market_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .expected
            .keys()
            .filter(|m| self.validated(m).is_some())
            .cloned()
            .collect();
        ids.sort();
        ids
    }
}
