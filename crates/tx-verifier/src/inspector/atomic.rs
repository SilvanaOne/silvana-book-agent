//! Atomic DvP settlement inspection.

use std::collections::BTreeMap;

use super::*;
use crate::decimal::parse_exact;
use crate::facts::{
    extract_facts, Decimal, FeeLeg, FlowKind, QuoteSide, TxFacts, SETTLE_CHOICE, VENUE_TEMPLATE,
};
use crate::types::ExpectedFee;

/// AtomicDvpSettle: the one settle of the agent's request, its signed fees and only
/// the outflows the quote explains; every finding observes first.
pub(super) fn inspect_atomic_settle(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::AtomicDvpSettle { user_party, quote_id, fee_party, .. } = expectation else {
        return Err(wrong_expectation("AtomicDvpSettle"));
    };
    let mut warnings = Vec::new();
    match extract_facts(prepared_transaction_bytes, user_party) {
        Ok(facts) => {
            observed.extend(settle_findings(&facts, expectation));
            let fees = facts.atomic_settle.as_ref().map_or(0, |s| s.fees.len());
            if fee_party.is_none() && fees > 0 {
                let (noun, verb) = if fees == 1 { ("fee", "is") } else { ("fees", "are") };
                warnings.push(format!(
                    "AtomicDvpSettle: no fee party is pinned (PARTY_FEE); {fees} settle {noun} {verb} checked against the signed request only"
                ));
            }
        }
        Err(e) => observed.push(format!("AtomicDvpSettle: the transaction facts are unreadable: {e}")),
    }
    let summary = format!(
        "AtomicDvpSettle checked: {} settles quote {}",
        short(user_party, 8),
        short(quote_id, 24)
    );
    debug!("TX INSPECT: {}", summary);
    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

fn sorted(cids: &[String]) -> Vec<&str> {
    let mut cids: Vec<&str> = cids.iter().map(String::as_str).collect();
    cids.sort_unstable();
    cids
}

fn settle_findings(facts: &TxFacts, expectation: &OperationExpectation) -> Vec<String> {
    let OperationExpectation::AtomicDvpSettle {
        user_party,
        venue_cid,
        quote_id,
        ticket_id,
        side,
        base_amount,
        quote_amount,
        lp_party,
        base_instrument_id,
        base_instrument_admin,
        quote_instrument_id,
        quote_instrument_admin,
        valid_until_micros,
        lp_input_holding_cids,
        user_input_holding_cids,
        expected_fees,
        fee_party,
        ..
    } = expectation
    else {
        return Vec::new();
    };
    let mut found = Vec::new();
    if facts.act_as != [user_party.as_str()] {
        found.push(format!("AtomicDvpSettle: act_as {:?} is not exactly the user", facts.act_as));
    }
    let one_root = facts.root_nodes == 1
        && facts.roots.len() == 1
        && facts.roots.first().is_some_and(|r| {
            r.template == VENUE_TEMPLATE && r.choice == SETTLE_CHOICE && r.contract_id == *venue_cid
        });
    if !one_root {
        found.push(format!(
            "AtomicDvpSettle: expected one root {SETTLE_CHOICE} on venue {}, got {} root nodes",
            short(venue_cid, 16),
            facts.root_nodes
        ));
    }
    let Some(s) = facts.atomic_settle.as_ref() else {
        found.push("AtomicDvpSettle: the transaction runs no settle".to_string());
        return found;
    };
    let buy = match side.as_str() {
        "Buy" => Some(true),
        "Sell" => Some(false),
        _ => None,
    };
    let expected_side = buy.map(|b| if b { QuoteSide::Buy } else { QuoteSide::Sell });
    if s.quote_id != *quote_id || s.ticket_id != *ticket_id || Some(s.side) != expected_side || s.user != *user_party {
        found.push(format!(
            "AtomicDvpSettle: the settled quote {} ({:?}, user {}) is not the requested quote {} ({side})",
            short(&s.quote_id, 24),
            s.side,
            short(&s.user, 16),
            short(quote_id, 24)
        ));
    }
    let base = parse_exact(base_amount);
    let quote = parse_exact(quote_amount);
    if base != Some(s.base_amount) || quote != Some(s.quote_amount) {
        found.push(format!(
            "AtomicDvpSettle: the settled amounts {} and {} are not the requested {base_amount} and {quote_amount}",
            s.base_amount, s.quote_amount
        ));
    }
    if *valid_until_micros <= 0 || s.valid_until_micros != *valid_until_micros {
        found.push(format!(
            "AtomicDvpSettle: the quote's validUntil {} is not the end {valid_until_micros} of the accepted quote window",
            s.valid_until_micros
        ));
    }
    if sorted(&s.user_input_holding_cids) != sorted(user_input_holding_cids) {
        found.push("AtomicDvpSettle: the user inputs are not the selected holdings".to_string());
    }
    if sorted(&s.lp_input_holding_cids) != sorted(lp_input_holding_cids) {
        found.push("AtomicDvpSettle: the LP inputs are not the quoted holdings".to_string());
    }
    match &s.venue {
        None => found.push("AtomicDvpSettle: the venue is not an input contract, so its LP is unchecked".to_string()),
        Some(v) => {
            let pair = v.base.admin == *base_instrument_admin
                && v.base.id == *base_instrument_id
                && v.quote.admin == *quote_instrument_admin
                && v.quote.id == *quote_instrument_id;
            if v.lp != *lp_party || !pair {
                found.push(format!(
                    "AtomicDvpSettle: the venue's LP {} and pair {}/{} are not the requested LP {} and pair {base_instrument_id}/{quote_instrument_id}",
                    short(&v.lp, 16),
                    v.base.id,
                    v.quote.id,
                    short(lp_party, 16)
                ));
            }
        }
    }
    found.extend(fee_findings(&s.fees, expected_fees, fee_party.as_deref()));

    // Outflows: the agent's own leg to the LP, the fees and the LP's change in its own instrument.
    let (Some(buy), Some(base), Some(quote)) = (buy, base, quote) else {
        return found;
    };
    let (own, proceeds) = if buy {
        ((quote_instrument_admin, quote_instrument_id, quote), (base_instrument_admin, base_instrument_id, base))
    } else {
        ((base_instrument_admin, base_instrument_id, base), (quote_instrument_admin, quote_instrument_id, quote))
    };
    let lp_change = (proceeds.0, proceeds.1);
    let mut legs: Vec<(&str, &str, &str, Decimal)> = vec![(lp_party.as_str(), own.0.as_str(), own.1.as_str(), own.2)];
    legs.extend(s.fees.iter().map(|f| (f.receiver.as_str(), f.admin.as_str(), f.id.as_str(), f.amount)));
    let mut used = vec![false; legs.len()];
    for flow in facts.outflows.iter().filter(|f| f.to != *user_party) {
        let transfer = matches!(flow.kind, FlowKind::Holding | FlowKind::PendingTransfer);
        let exact = legs.iter().zip(used.iter_mut()).find(|((to, admin, id, amount), used)| {
            !**used
                && transfer
                && flow.to == *to
                && flow.instrument_admin == *admin
                && flow.instrument_id == *id
                && flow.amount == *amount
        });
        if let Some((_, used)) = exact {
            *used = true;
            continue;
        }
        let change = transfer
            && flow.to == *lp_party
            && flow.instrument_admin == *lp_change.0
            && flow.instrument_id == *lp_change.1;
        if !change {
            found.push(format!(
                "AtomicDvpSettle: an outflow of {} {} ({:?}) to {} is not the agent's leg, a fee or the LP's change",
                flow.amount,
                flow.instrument_id,
                flow.kind,
                short(&flow.to, 16)
            ));
        }
    }
    if used.first() != Some(&true) {
        found.push("AtomicDvpSettle: the agent's leg to the LP is not among the outflows".to_string());
    }
    // The LP's change is unbounded, so the user's own holdings must balance exactly.
    found.extend(conservation_findings(
        facts,
        (own.0.as_str(), own.1.as_str(), own.2),
        (proceeds.0.as_str(), proceeds.1.as_str(), proceeds.2),
        &s.fees,
    ));
    found
}

/// Adds `amount` to the total of one instrument, or subtracts it; `false` on overflow.
fn tally<'a>(
    totals: &mut BTreeMap<(&'a str, &'a str), Decimal>,
    instrument: (&'a str, &'a str),
    amount: Decimal,
    add: bool,
) -> bool {
    let total = totals.entry(instrument).or_insert(Decimal::ZERO);
    let next = if add { total.checked_add(amount) } else { total.checked_sub(amount) };
    next.map(|n| *total = n).is_some()
}

/// Per instrument, the user's consumed holdings less the unlocked holdings it gets are
/// exactly its leg and the fees less its proceeds.
fn conservation_findings(
    facts: &TxFacts,
    own: (&str, &str, Decimal),
    proceeds: (&str, &str, Decimal),
    fees: &[FeeLeg],
) -> Vec<String> {
    let mut lost = BTreeMap::new();
    let mut owed = BTreeMap::new();
    let mut fits = true;
    for h in &facts.consumed {
        fits &= tally(&mut lost, (&h.instrument_admin, &h.instrument_id), h.amount, true);
    }
    for h in facts.inflows.iter().filter(|h| !h.locked) {
        fits &= tally(&mut lost, (&h.instrument_admin, &h.instrument_id), h.amount, false);
    }
    fits &= tally(&mut owed, (own.0, own.1), own.2, true);
    fits &= tally(&mut owed, (proceeds.0, proceeds.1), proceeds.2, false);
    for f in fees {
        fits &= tally(&mut owed, (&f.admin, &f.id), f.amount, true);
    }
    if !fits {
        return vec!["AtomicDvpSettle: the user's holdings overflow a decimal".to_string()];
    }
    for instrument in owed.keys() {
        lost.entry(*instrument).or_insert(Decimal::ZERO);
    }
    lost.iter()
        .filter_map(|(instrument, have)| {
            let want = owed.get(instrument).copied().unwrap_or(Decimal::ZERO);
            (*have != want).then(|| {
                format!(
                    "AtomicDvpSettle: the user's net outflow of {have} {} is not its leg and fees less its proceeds, {want}",
                    instrument.1
                )
            })
        })
        .collect()
}

/// The settle pays exactly the request's signed fees, to the pinned fee party.
fn fee_findings(fees: &[FeeLeg], expected: &[ExpectedFee], fee_party: Option<&str>) -> Vec<String> {
    let mut found = Vec::new();
    let mut used = vec![false; expected.len()];
    for fee in fees {
        if let Some(party) = fee_party.filter(|p| fee.receiver != *p) {
            found.push(format!(
                "AtomicDvpSettle: a settle fee goes to {}, not to the pinned fee party {}",
                short(&fee.receiver, 16),
                short(party, 16)
            ));
        }
        let signed = expected.iter().zip(used.iter_mut()).find(|(e, used)| {
            !**used
                && e.receiver == fee.receiver
                && e.instrument_admin == fee.admin
                && e.instrument_id == fee.id
                && parse_exact(&e.amount) == Some(fee.amount)
        });
        match signed {
            Some((_, used)) => *used = true,
            None => found.push(format!(
                "AtomicDvpSettle: the settle fee of {} {} to {} is not a signed fee of the request",
                fee.amount,
                fee.id,
                short(&fee.receiver, 16)
            )),
        }
    }
    let missing = used.iter().filter(|u| !**u).count();
    if missing > 0 {
        let (noun, verb) = if missing == 1 { ("fee", "is") } else { ("fees", "are") };
        found.push(format!("AtomicDvpSettle: {missing} signed {noun} of the request {verb} not in the settle"));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{ExerciseFact, HoldingFact, Instrument, SettleFacts, ValueFlow, VenueTerms};

    const USER: &str = "user::1220aa";
    const LP: &str = "lp::1220bb";
    const FEE: &str = "fee::1220ff";
    const DSO: &str = "dso::1220dd";
    const REG: &str = "reg::1220ee";

    fn dec(s: &str) -> Decimal {
        parse_exact(s).unwrap()
    }

    fn held(cid: &str, admin: &str, id: &str, amount: &str) -> HoldingFact {
        HoldingFact { contract_id: cid.into(), instrument_admin: admin.into(), instrument_id: id.into(), amount: dec(amount), locked: false }
    }

    fn to(party: &str, admin: &str, id: &str, amount: &str) -> ValueFlow {
        ValueFlow { to: party.into(), instrument_admin: admin.into(), instrument_id: id.into(), amount: dec(amount), kind: FlowKind::Holding }
    }

    /// A buy of 100 CC for 10 USDCx with one signed fee, from the user's `consumed` inputs.
    fn buy_cc(
        fee: (&str, &str, &str),
        consumed: Vec<HoldingFact>,
        inflows: Vec<HoldingFact>,
        outflows: Vec<ValueFlow>,
    ) -> (TxFacts, OperationExpectation) {
        let (admin, id, amount) = fee;
        let inputs: Vec<String> = consumed.iter().map(|h| h.contract_id.clone()).collect();
        let facts = TxFacts {
            act_as: vec![USER.into()],
            roots: vec![ExerciseFact { template: VENUE_TEMPLATE.into(), choice: SETTLE_CHOICE.into(), contract_id: "v1".into() }],
            root_nodes: 1,
            outflows,
            inflows,
            consumed,
            atomic_settle: Some(SettleFacts {
                venue_cid: "v1".into(),
                quote_id: "q1".into(),
                ticket_id: String::new(),
                user: USER.into(),
                side: QuoteSide::Buy,
                base_amount: dec("100"),
                quote_amount: dec("10"),
                valid_until_micros: 7,
                lp_input_holding_cids: vec!["lp1".into()],
                user_input_holding_cids: inputs.clone(),
                fees: vec![FeeLeg { receiver: FEE.into(), admin: admin.into(), id: id.into(), amount: dec(amount) }],
                venue: Some(VenueTerms {
                    lp: LP.into(),
                    provider: LP.into(),
                    pair_name: "CC/USDCx".into(),
                    base: Instrument { admin: DSO.into(), id: "Amulet".into() },
                    quote: Instrument { admin: REG.into(), id: "USDCx".into() },
                }),
            }),
            ..TxFacts::default()
        };
        let expectation = OperationExpectation::AtomicDvpSettle {
            user_party: USER.into(),
            venue_cid: "v1".into(),
            template_id: "#atomic-dvp-v2:AtomicDVP:AtomicDVP".into(),
            quote_id: "q1".into(),
            ticket_id: String::new(),
            ticket_cid: None,
            side: "Buy".into(),
            base_amount: "100".into(),
            quote_amount: "10".into(),
            lp_party: LP.into(),
            base_instrument_id: "Amulet".into(),
            base_instrument_admin: DSO.into(),
            quote_instrument_id: "USDCx".into(),
            quote_instrument_admin: REG.into(),
            valid_until_micros: 7,
            lp_input_holding_cids: vec!["lp1".into()],
            user_input_holding_cids: inputs,
            expected_fees: vec![ExpectedFee {
                receiver: FEE.into(),
                instrument_admin: admin.into(),
                instrument_id: id.into(),
                amount: amount.into(),
            }],
            fee_party: Some(FEE.into()),
        };
        (facts, expectation)
    }

    #[test]
    fn a_settle_fee_in_another_instrument_is_judged_by_the_signed_fees() {
        let usdcx_fee = || {
            buy_cc(
                (REG, "USDCx", "0.15"),
                vec![held("u1", REG, "USDCx", "20")],
                vec![held("c1", DSO, "Amulet", "100"), held("c2", REG, "USDCx", "9.85")],
                vec![to(LP, REG, "USDCx", "10"), to(FEE, REG, "USDCx", "0.15"), to(LP, DSO, "Amulet", "900")],
            )
        };
        let (facts, e) = usdcx_fee();
        assert_eq!(settle_findings(&facts, &e), Vec::<String>::new());
        // The fee stays bound to the signed one.
        let (facts, mut e) = usdcx_fee();
        if let OperationExpectation::AtomicDvpSettle { expected_fees, .. } = &mut e {
            expected_fees[0].amount = "0.16".into();
        }
        let found = settle_findings(&facts, &e);
        assert!(found.iter().any(|f| f.contains("not a signed fee")), "{found:?}");
        assert!(found.iter().any(|f| f.ends_with(": 1 signed fee of the request is not in the settle")), "{found:?}");
    }

    #[test]
    fn lp_change_never_absorbs_the_users_own_inputs() {
        // A 1 CC fee funded by the user's 5000 CC input: the change of 4999 CC is the user's.
        let consumed = || vec![held("u1", REG, "USDCx", "10"), held("u2", DSO, "Amulet", "5000")];
        let pays = || vec![to(LP, REG, "USDCx", "10"), to(FEE, DSO, "Amulet", "1"), to(LP, DSO, "Amulet", "900")];
        let fee = (DSO, "Amulet", "1");
        let honest = || vec![held("c1", DSO, "Amulet", "100"), held("c2", DSO, "Amulet", "4999")];
        let (facts, e) = buy_cc(fee, consumed(), honest(), pays());
        assert_eq!(settle_findings(&facts, &e), Vec::<String>::new());
        // The same 4999 CC sent to the LP passes for its change unless the user side balances.
        let mut outflows = pays();
        outflows.push(to(LP, DSO, "Amulet", "4999"));
        let (facts, e) = buy_cc(fee, consumed(), vec![held("c1", DSO, "Amulet", "100")], outflows);
        let found = settle_findings(&facts, &e);
        assert!(found.iter().any(|f| f.contains("net outflow of 4900 Amulet")), "{found:?}");
        // Short proceeds, and change locked to others, do not balance either.
        let short = vec![held("c1", DSO, "Amulet", "99"), held("c2", DSO, "Amulet", "4999")];
        let (facts, e) = buy_cc(fee, consumed(), short, pays());
        assert!(!settle_findings(&facts, &e).is_empty());
        let mut locked = honest();
        locked[1].locked = true;
        let (facts, e) = buy_cc(fee, consumed(), locked, pays());
        assert!(!settle_findings(&facts, &e).is_empty());
    }
}
