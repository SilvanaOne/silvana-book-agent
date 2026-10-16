//! `--amount all` for the transfer commands: the full unlocked balance, sent as an
//! exact amount with the summed holdings pinned as inputs.

use anyhow::{Result, anyhow};
use rust_decimal::Decimal;

use crate::acs_worker::parse_acs_holdings;
use crate::holdings_cache::{TEMPLATE_HOLDING, instrument_key};
use crate::ledger_client::DAppProviderClient;

/// Most inputs `--amount all` puts in one transfer when `--count` is not given.
pub const MAX_ALL_INPUTS: usize = 100;

/// True when `--amount` asks for the full unlocked balance.
pub fn is_all_amount(amount: &str) -> bool {
    amount.trim().eq_ignore_ascii_case("all")
}

/// Holdings to send for `--amount all` (the `count` biggest when given) and their total.
pub fn select_all(
    mut holdings: Vec<(String, Decimal)>,
    count: Option<usize>,
    label: &str,
    hint: &str,
) -> Result<(Vec<String>, Decimal)> {
    if holdings.is_empty() {
        return Err(anyhow!(
            "--amount all: no unlocked {label} holdings to send"
        ));
    }
    holdings.sort_by(|a, b| b.1.cmp(&a.1));

    let take = match count {
        Some(n) => n,
        None if holdings.len() > MAX_ALL_INPUTS => {
            let biggest =
                agent_logic::num::dec_sum(holdings.iter().take(MAX_ALL_INPUTS).map(|(_, a)| *a))
                    .ok_or_else(|| anyhow!("{label} holding total is out of range"))?;
            return Err(anyhow!(
                "--amount all needs {} unlocked {label} holdings, more than the \
                 {MAX_ALL_INPUTS}-input limit of one transfer; the {MAX_ALL_INPUTS} biggest \
                 hold {biggest} {label} — {hint}",
                holdings.len()
            ));
        }
        None => holdings.len(),
    };
    holdings.truncate(take);

    let total = agent_logic::num::dec_sum(holdings.iter().map(|(_, a)| *a))
        .ok_or_else(|| anyhow!("{label} holding total is out of range"))?;
    if total <= Decimal::ZERO {
        return Err(anyhow!(
            "--amount all: no positive unlocked {label} balance to send ({} holding(s) selected)",
            holdings.len()
        ));
    }
    Ok((holdings.into_iter().map(|(cid, _)| cid).collect(), total))
}

/// `--amount all` over the party's unlocked CC amulets, as returned by GetAmulets.
pub async fn all_amulets(
    client: &mut DAppProviderClient,
    count: Option<usize>,
    hint: &str,
) -> Result<(Vec<String>, Decimal)> {
    let amulets = client.get_amulets().await?;
    select_all(
        amulets
            .into_iter()
            .map(|a| (a.contract_id, a.amount))
            .collect(),
        count,
        "CC",
        hint,
    )
}

/// `--amount all` over the party's unlocked holdings of one utility instrument.
/// Fetches only the Holding template, never the whole active contract set.
pub async fn all_holdings(
    client: &mut DAppProviderClient,
    party_id: &str,
    admin: &str,
    wire_id: &str,
    label: &str,
    count: Option<usize>,
    hint: &str,
) -> Result<(Vec<String>, Decimal)> {
    let contracts = client
        .get_active_contracts(&[TEMPLATE_HOLDING.to_string()])
        .await?;
    let key = instrument_key(admin, wire_id);
    let holdings = parse_acs_holdings(contracts, party_id)
        .into_iter()
        .filter(|h| h.instrument == key)
        .map(|h| (h.contract_id, h.amount))
        .collect();
    select_all(holdings, count, label, hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(cid: &str, amount: &str) -> (String, Decimal) {
        (cid.to_string(), amount.parse().unwrap())
    }

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn ones(n: usize) -> Vec<(String, Decimal)> {
        (0..n).map(|i| h(&format!("c{i}"), "1")).collect()
    }

    #[test]
    fn all_keyword_matches_any_case_and_padding() {
        assert!(is_all_amount("all"));
        assert!(is_all_amount("ALL"));
        assert!(is_all_amount(" All "));
        assert!(!is_all_amount("al"));
        assert!(!is_all_amount("10"));
        assert!(!is_all_amount(""));
    }

    #[test]
    fn takes_every_holding_biggest_first_and_sums_exactly() {
        let (cids, total) = select_all(
            vec![h("a", "40.1"), h("b", "0.0345900874"), h("c", "0.1")],
            None,
            "TKN",
            "hint",
        )
        .unwrap();
        assert_eq!(cids, ["a", "c", "b"]);
        assert_eq!(total, dec("40.2345900874"));
    }

    #[test]
    fn count_takes_the_biggest() {
        let (cids, total) = select_all(
            vec![h("a", "40.1"), h("b", "0.0345900874"), h("c", "0.1")],
            Some(2),
            "TKN",
            "hint",
        )
        .unwrap();
        assert_eq!(cids, ["a", "c"]);
        assert_eq!(total, dec("40.2"));
    }

    #[test]
    fn over_limit_without_count_is_refused_with_numbers_and_hint() {
        let err = select_all(ones(MAX_ALL_INPUTS + 1), None, "TKN", "re-run with --count")
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs 101 unlocked TKN holdings"), "{err}");
        assert!(err.contains("100-input limit"), "{err}");
        assert!(err.contains("the 100 biggest hold 100 TKN"), "{err}");
        assert!(err.contains("re-run with --count"), "{err}");
    }

    #[test]
    fn explicit_count_above_limit_is_honoured() {
        let (cids, total) = select_all(
            ones(MAX_ALL_INPUTS + 1),
            Some(MAX_ALL_INPUTS + 1),
            "TKN",
            "hint",
        )
        .unwrap();
        assert_eq!(cids.len(), MAX_ALL_INPUTS + 1);
        assert_eq!(total, dec("101"));
    }

    #[test]
    fn empty_and_non_positive_selections_are_refused() {
        let err = select_all(Vec::new(), None, "TKN", "hint")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no unlocked TKN holdings"), "{err}");
        assert!(select_all(vec![h("z", "0")], None, "TKN", "hint").is_err());
        assert!(select_all(vec![h("a", "5")], Some(0), "TKN", "hint").is_err());
    }
}
