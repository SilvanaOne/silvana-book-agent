//! Which categories a new policy widens, and the delay before it takes effect.

use super::{Allowlist, CapValue, Limits, PolicyDocument, Rules};
use crate::canon::time::UtcTime;
use crate::canon::values::Amount;
use crate::state::add_secs;

/// The categories in which a policy grants more than its baseline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Widening {
    pub transfers: bool,
    pub trading: bool,
    pub other: bool,
}

impl Widening {
    pub fn any(&self) -> bool {
        self.transfers || self.trading || self.other
    }
}

fn switched_on(old: bool, new: bool) -> bool {
    !old && new
}

fn gains_member<T: PartialEq>(old: &Allowlist<T>, new: &Allowlist<T>) -> bool {
    if new.any {
        return !old.any;
    }
    !old.any && new.items.iter().any(|item| !old.items.contains(item))
}

fn cap_rises(old: Option<Amount>, new: Option<Amount>) -> bool {
    CapValue::of_optional(new) > CapValue::of_optional(old)
}

/// A rise of the unlisted cap, or of any instrument named in either list.
fn limits_rise(old: &Limits, new: &Limits) -> bool {
    new.unlisted_cap() > old.unlisted_cap()
        || old
            .items
            .iter()
            .chain(new.items.iter())
            .any(|item| new.cap_for(&item.instrument) > old.cap_for(&item.instrument))
}

pub fn widening(old: &Rules, new: &Rules) -> Widening {
    let (ot, nt) = (&old.transfers, &new.transfers);
    let transfers = switched_on(ot.enabled, nt.enabled)
        || (ot.directed_only && !nt.directed_only)
        || gains_member(&ot.recipients, &nt.recipients)
        || gains_member(&ot.eth_destinations, &nt.eth_destinations)
        || limits_rise(&ot.per_transfer, &nt.per_transfer)
        || limits_rise(&ot.per_day, &nt.per_day)
        || new.delays.transfers_secs < old.delays.transfers_secs;
    let (og, ng) = (&old.trading, &new.trading);
    let trading = switched_on(og.enabled, ng.enabled)
        || switched_on(og.lp_enabled, ng.lp_enabled)
        || gains_member(&og.markets, &ng.markets)
        || gains_member(&og.instruments, &ng.instruments)
        || limits_rise(&og.per_trade, &ng.per_trade)
        || limits_rise(&og.per_day, &ng.per_day)
        || new.delays.trading_secs < old.delays.trading_secs;
    let other = (old.paused && !new.paused)
        || switched_on(old.maintenance.enabled, new.maintenance.enabled)
        || switched_on(old.locks.enabled, new.locks.enabled)
        || switched_on(old.usdcx.enabled, new.usdcx.enabled)
        || cap_rises(
            old.maintenance.traffic_cc_per_day,
            new.maintenance.traffic_cc_per_day,
        )
        || limits_rise(&old.locks.per_lock, &new.locks.per_lock)
        || cap_rises(old.fees.cc_per_consent, new.fees.cc_per_consent)
        || cap_rises(old.fees.cc_per_day, new.fees.cc_per_day)
        || new.delays.other_secs < old.delays.other_secs;
    Widening {
        transfers,
        trading,
        other,
    }
}

/// The largest baseline delay among the categories `new` widens, or 0.
pub fn required_delay_secs(baseline: &Rules, new: &Rules) -> u64 {
    let widened = widening(baseline, new);
    let delays = &baseline.delays;
    [
        (widened.transfers, delays.transfers_secs),
        (widened.trading, delays.trading_secs),
        (widened.other, delays.other_secs),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, secs)| secs)
    .max()
    .unwrap_or(0)
}

/// max(accepted_at + the required delay, not_before); `None` past the time range.
pub fn expected_effective_at(
    accepted_at: UtcTime,
    baseline: &Rules,
    doc: &PolicyDocument,
) -> Option<UtcTime> {
    let delayed = add_secs(accepted_at, required_delay_secs(baseline, &doc.rules))?;
    Some(match doc.not_before {
        Some(not_before) if not_before > delayed => not_before,
        _ => delayed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::values::Network;
    use crate::policy::{DEFAULT_LINKED_RULES, LimitItem, PolicyInstrument, Unlisted};
    use crate::testkit;

    fn amount(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }

    fn cc_limit(value: &str) -> Limits {
        Limits {
            unlisted: Unlisted::Unlimited,
            items: vec![LimitItem {
                instrument: PolicyInstrument::Cc,
                amount: amount(value),
            }],
        }
    }

    fn check(old: &Rules, new: &Rules) -> (bool, bool, bool) {
        let w = widening(old, new);
        (w.transfers, w.trading, w.other)
    }

    #[test]
    fn narrowing_widens_nothing() {
        let base = DEFAULT_LINKED_RULES;
        let mut narrow = base.clone();
        narrow.paused = true;
        narrow.transfers.enabled = false;
        narrow.trading.lp_enabled = false;
        narrow.trading.per_trade = cc_limit("10");
        narrow.fees.cc_per_day = Some(amount("1"));
        narrow.delays.transfers_secs = 172_800;
        assert!(!widening(&base, &narrow).any());
        assert_eq!(required_delay_secs(&base, &narrow), 0);
        assert_eq!(check(&narrow, &base), (true, true, true));
    }

    #[test]
    fn each_category_has_its_own_triggers() {
        let base = DEFAULT_LINKED_RULES;
        let mut undirected = base.clone();
        undirected.transfers.directed_only = false;
        assert_eq!(check(&base, &undirected), (true, false, false));
        assert_eq!(required_delay_secs(&base, &undirected), 86_400);
        let mut faster = base.clone();
        faster.delays.transfers_secs = 3600;
        assert_eq!(check(&base, &faster), (true, false, false));
        assert_eq!(required_delay_secs(&base, &faster), 86_400);
        let mut capped = base.clone();
        capped.trading.per_day = cc_limit("10");
        let mut raised = capped.clone();
        raised.trading.per_day = cc_limit("10.5");
        assert_eq!(check(&capped, &raised), (false, true, false));
        assert_eq!(required_delay_secs(&capped, &raised), 0);
        let mut paused = base.clone();
        paused.paused = true;
        assert_eq!(check(&paused, &base), (false, false, true));
        let changes: [fn(&mut Rules); 6] = [
            |r: &mut Rules| r.locks.enabled = false,
            |r: &mut Rules| r.usdcx.enabled = false,
            |r: &mut Rules| r.maintenance.enabled = false,
            |r: &mut Rules| r.fees.cc_per_consent = Some(Amount::ZERO),
            |r: &mut Rules| r.maintenance.traffic_cc_per_day = Some(Amount::ZERO),
            |r: &mut Rules| r.locks.per_lock = cc_limit("1"),
        ];
        for change in changes {
            let mut old = base.clone();
            change(&mut old);
            assert_eq!(check(&old, &base), (false, false, true));
        }
        let mut slow_other = base.clone();
        slow_other.delays.other_secs = 600;
        let mut unpaused = slow_other.clone();
        unpaused.paused = true;
        assert_eq!(required_delay_secs(&unpaused, &slow_other), 600);
    }

    #[test]
    fn allowlists_widen_when_they_gain_a_member() {
        let base = DEFAULT_LINKED_RULES;
        let alice = testkit::agent_party();
        let mut listed = base.clone();
        listed.transfers.recipients = Allowlist {
            any: false,
            items: vec![alice.clone()],
        };
        assert_eq!(check(&listed, &base), (true, false, false));
        assert_eq!(check(&base, &listed), (false, false, false));
        let mut empty = listed.clone();
        empty.transfers.recipients.items.clear();
        assert_eq!(check(&empty, &listed), (true, false, false));
        assert_eq!(check(&listed, &empty), (false, false, false));
        let mut markets = base.clone();
        markets.trading.markets = Allowlist {
            any: false,
            items: Vec::new(),
        };
        assert_eq!(check(&markets, &base), (false, true, false));
    }

    #[test]
    fn caps_order_denied_below_amounts_below_unlimited() {
        assert!(CapValue::Denied < CapValue::Amount(Amount::ZERO));
        assert!(CapValue::Amount(amount("5")) < CapValue::Amount(amount("10")));
        assert!(CapValue::Amount(amount("99999999999999999")) < CapValue::Unlimited);
        let deny = Limits {
            unlisted: Unlisted::Deny,
            items: Vec::new(),
        };
        let deny_listed = Limits {
            unlisted: Unlisted::Deny,
            items: cc_limit("1").items,
        };
        let mut old = DEFAULT_LINKED_RULES;
        old.transfers.per_transfer = deny.clone();
        let mut new = old.clone();
        new.transfers.per_transfer = deny_listed.clone();
        assert_eq!(check(&old, &new), (true, false, false));
        assert_eq!(check(&new, &old), (false, false, false));
        // Dropping a listed cap falls back to the unlisted one.
        let mut unlimited_listed = old.clone();
        unlimited_listed.transfers.per_transfer = Limits {
            unlisted: Unlisted::Unlimited,
            items: cc_limit("1").items,
        };
        let mut unlimited = old.clone();
        unlimited.transfers.per_transfer = Limits::UNLIMITED;
        assert_eq!(check(&unlimited_listed, &unlimited), (true, false, false));
    }

    #[test]
    fn not_before_later_than_the_delay_wins() {
        let accepted = testkit::t("2026-11-22T12:00:00Z");
        let mut doc = crate::policy::PolicyDocument {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            version: 1,
            prev_policy_hash: crate::canon::values::Hash32([0; 32]),
            issued_at: accepted,
            expires_at: testkit::t("2026-11-22T13:00:00Z"),
            not_before: None,
            rules: DEFAULT_LINKED_RULES,
        };
        let base = DEFAULT_LINKED_RULES;
        assert_eq!(expected_effective_at(accepted, &base, &doc), Some(accepted));
        doc.not_before = Some(testkit::t("2026-11-24T00:00:00Z"));
        assert_eq!(expected_effective_at(accepted, &base, &doc), doc.not_before);
        doc.rules.transfers.directed_only = false;
        doc.not_before = Some(testkit::t("2026-11-23T00:00:00Z"));
        assert_eq!(
            expected_effective_at(accepted, &base, &doc),
            Some(testkit::t("2026-11-23T12:00:00Z"))
        );
        let end = testkit::t("9999-12-31T00:00:00Z");
        assert_eq!(expected_effective_at(end, &base, &doc), None);
    }
}
