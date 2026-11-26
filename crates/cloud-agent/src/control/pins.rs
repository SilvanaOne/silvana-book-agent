//! The platform parties an agent pins on first use (fee, prepaid traffic, traffic fee):
//! written only where the agent's env sets none, never replaced or removed.

#![cfg_attr(
    not(test),
    allow(renamed_and_removed_lints),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::unchecked_duration_subtraction,
        clippy::arithmetic_side_effects,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::dbg_macro,
        clippy::disallowed_methods
    ),
    warn(renamed_and_removed_lints)
)]

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use agent_logic::config::BaseConfig;
use anyhow::{Result, anyhow};
use orderbook_proto::ledger::GetAgentConfigRequest;
use orderbook_proto::ledger::d_app_provider_service_client::DAppProviderServiceClient;
use tracing::{info, warn};

use super::PlatformParties;

/// The env keys of the platform parties.
pub const PIN_KEYS: [&str; 3] = ["PARTY_FEE", "PARTY_PREPAID_TRAFFIC", TRAFFIC_FEE_KEY];
/// The pin only top-ups paid in a token other than CC need; unset, it is reported only
/// where such a token is listed or the agent config names it.
pub const TRAFFIC_FEE_KEY: &str = "PARTY_TRAFFIC_FEE";
/// Bound on reading the agent config for the pins.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// How one pin stands after [`pin_missing`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinState {
    /// Written now.
    Pinned(String),
    /// Set already and kept; `offered` is another value the agent config named.
    Kept {
        local: String,
        offered: Option<String>,
    },
    /// Set nowhere.
    Unset,
}

/// The pins an agent config template names, each checked as a party id.
pub fn template_pins(env: &str) -> Result<Vec<(&'static str, String)>> {
    let mut pins = Vec::new();
    for key in PIN_KEYS {
        let named = env.lines().find_map(|line| {
            let (name, value) = line.trim().split_once('=')?;
            (name.trim() == key).then(|| value.trim().trim_matches('"').trim().to_string())
        });
        let Some(party) = named.filter(|v| !v.is_empty()) else {
            continue;
        };
        agent_control::PartyId::parse(&party).map_err(|_| {
            anyhow!(
                "refusing to pin {key}: the agent config names {party:?}, which is not a party id"
            )
        })?;
        pins.push((key, party));
    }
    Ok(pins)
}

/// The value `key` has in the process or in the env file; blank counts as unset.
fn local_value(
    env_file: &Path,
    key: &str,
    process: &dyn Fn(&str) -> Option<OsString>,
) -> Option<String> {
    let set = |v: Option<String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    set(process(key).and_then(|v| v.into_string().ok()))
        .or_else(|| set(crate::read_env_value(env_file, key)))
}

/// The pin keys the agent's env does not set.
pub fn missing(env_file: &Path, process: &dyn Fn(&str) -> Option<OsString>) -> Vec<&'static str> {
    PIN_KEYS
        .into_iter()
        .filter(|key| local_value(env_file, key, process).is_none())
        .collect()
}

/// Writes each of `pins` the agent's env does not set yet; a set value is never replaced.
pub fn pin_missing(
    env_file: &Path,
    pins: &[(&'static str, String)],
    process: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Vec<(&'static str, PinState)>> {
    let mut states = Vec::new();
    for key in PIN_KEYS {
        let offered = pins.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        let state = match (local_value(env_file, key, process), offered) {
            (Some(local), offered) => {
                let offered = offered.filter(|v| *v != local);
                PinState::Kept { local, offered }
            }
            (None, Some(party)) => {
                crate::upsert_env_value(env_file, key, &party)?;
                PinState::Pinned(party)
            }
            (None, None) if key == TRAFFIC_FEE_KEY => continue,
            (None, None) => PinState::Unset,
        };
        states.push((key, state));
    }
    Ok(states)
}

/// What a linked agent refuses while `key` is unset.
fn refused_payments(key: &str) -> &'static str {
    match key {
        "PARTY_FEE" => "to sign its fee payments and to take a swap quote with a fee",
        TRAFFIC_FEE_KEY => "to sign its prepaid traffic top-ups paid in a token other than CC",
        _ => "to sign its prepaid traffic top-ups",
    }
}

/// Whether a top-up token list needs the traffic-fee pin: it lists a token other than CC.
pub fn traffic_fee_needed(topup_tokens: &[String]) -> bool {
    topup_tokens
        .iter()
        .any(|token| !token.eq_ignore_ascii_case(agent_logic::config::CC_FEE_TOKEN))
}

/// How the pins stand, one line each.
pub fn write_states(out: &mut dyn Write, states: &[(&'static str, PinState)]) {
    for (key, state) in states {
        let _ = match state {
            PinState::Pinned(party) => writeln!(out, "  {key}={party} (pinned now)"),
            PinState::Kept {
                local,
                offered: None,
            } => writeln!(out, "  {key}={local} (pinned before, kept)"),
            PinState::Kept {
                local,
                offered: Some(other),
            } => writeln!(
                out,
                "  {key}={local} (pinned before, kept; the agent config names {other}, which is not used)"
            ),
            PinState::Unset => writeln!(
                out,
                "  {key} is not set: a linked agent refuses {}; set it from a trusted source",
                refused_payments(key)
            ),
        };
    }
}

/// The pins the agent config at `rpc_url` names.
pub async fn fetch(rpc_url: &str) -> Result<Vec<(&'static str, String)>> {
    let read = async {
        let channel = crate::create_raw_channel(rpc_url).await?;
        let mut client =
            DAppProviderServiceClient::new(channel).max_decoding_message_size(16 * 1024 * 1024);
        let response = client
            .get_agent_config(GetAgentConfigRequest {})
            .await
            .map_err(|status| {
                anyhow!(
                    "GetAgentConfig failed: {:?} {}",
                    status.code(),
                    status.message()
                )
            })?;
        template_pins(&response.into_inner().env)
    };
    tokio::time::timeout(FETCH_TIMEOUT, read)
        .await
        .map_err(|_| anyhow!("the agent config was not read within {FETCH_TIMEOUT:?}"))?
}

/// The keys a link would pin, with the values the agent config names (the traffic-fee pin
/// only when named); empty when the env sets them all. A failed read leaves values unknown.
pub async fn for_link(env_file: &Path, rpc_url: &str) -> Vec<(&'static str, Option<String>)> {
    let missing = missing(env_file, &|name| std::env::var_os(name));
    if missing.is_empty() {
        return Vec::new();
    }
    let offered = fetch(rpc_url).await.unwrap_or_else(|e| {
        warn!("agent control: the platform parties could not be read from the agent config: {e:#}");
        Vec::new()
    });
    missing
        .into_iter()
        .map(|key| {
            (
                key,
                offered
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.clone()),
            )
        })
        .filter(|(key, value)| *key != TRAFFIC_FEE_KEY || value.is_some())
        .collect()
}

/// The pins of a link, shown before its confirmation.
pub fn write_offer(out: &mut dyn Write, offer: &[(&'static str, Option<String>)]) {
    if offer.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "Platform parties pinned with the link (from the agent config; a pinned value is never replaced):"
    );
    for (key, value) in offer {
        let _ = match value {
            Some(party) => writeln!(out, "  {key}={party}"),
            None => writeln!(
                out,
                "  {key}: not named; while it is unset a linked agent refuses {}",
                refused_payments(key)
            ),
        };
    }
}

/// Writes the pins of a confirmed link; a value set meanwhile is kept.
pub fn write_after_link(
    out: &mut dyn Write,
    env_file: &Path,
    offer: &[(&'static str, Option<String>)],
) {
    if offer.is_empty() {
        return;
    }
    let pins: Vec<(&'static str, String)> = offer
        .iter()
        .filter_map(|(key, value)| value.clone().map(|v| (*key, v)))
        .collect();
    match pin_missing(env_file, &pins, &|name| std::env::var_os(name)) {
        Ok(states) => {
            let _ = writeln!(out, "Platform parties in {}:", env_file.display());
            write_states(out, &states);
            if states
                .iter()
                .any(|(_, state)| matches!(state, PinState::Pinned(_)))
            {
                let _ = writeln!(
                    out,
                    "A running agent picks up the pins written now on its next poller round."
                );
            }
        }
        Err(e) => {
            let _ = writeln!(
                out,
                "The platform parties were not written to {} ({e:#}); set PARTY_FEE, PARTY_PREPAID_TRAFFIC and PARTY_TRAFFIC_FEE there by hand.",
                env_file.display()
            );
        }
    }
}

/// `current` with each unset pin taken from `env_file`; a set pin is never replaced, and a
/// value that is not a party id stays unset (logged with `announce`).
pub fn fill_unset(env_file: &Path, current: &PlatformParties, announce: bool) -> PlatformParties {
    let mut next = current.clone();
    for (key, slot) in [
        ("PARTY_FEE", &mut next.fee),
        ("PARTY_PREPAID_TRAFFIC", &mut next.prepaid_traffic),
        (TRAFFIC_FEE_KEY, &mut next.traffic_fee),
    ] {
        if slot.is_some() {
            continue;
        }
        match local_value(env_file, key, &|_| None) {
            Some(party) if agent_control::PartyId::parse(&party).is_ok() => {
                info!(
                    "agent control: {key}={party} is pinned from {}",
                    env_file.display()
                );
                *slot = Some(party);
            }
            Some(party) if announce => warn!(
                "agent control: {key}={party:?} in {} is not a party id; it stays unpinned",
                env_file.display()
            ),
            _ => {}
        }
    }
    next
}

/// The startup warning of a linked agent whose env lacks a pin it needs; None with them set.
pub fn unset_warning(config: &BaseConfig) -> Option<String> {
    unset_warning_of(&PlatformParties::from_config(config), &config.prepaid_traffic_topup_tokens)
}

/// [`unset_warning`] for the pins in force; the traffic-fee pin counts only when
/// `topup_tokens` lists a token other than CC.
pub fn unset_warning_of(parties: &PlatformParties, topup_tokens: &[String]) -> Option<String> {
    let needs_traffic_fee = traffic_fee_needed(topup_tokens);
    let unset: Vec<&str> = [
        ("PARTY_FEE", &parties.fee, true),
        ("PARTY_PREPAID_TRAFFIC", &parties.prepaid_traffic, true),
        (TRAFFIC_FEE_KEY, &parties.traffic_fee, needs_traffic_fee),
    ]
    .into_iter()
    .filter(|(_, value, needed)| *needed && value.is_none())
    .map(|(key, _, _)| key)
    .collect();
    if unset.is_empty() {
        return None;
    }
    let refused: Vec<&str> = unset.iter().map(|key| refused_payments(key)).collect();
    Some(format!(
        "{} not pinned: this linked agent refuses {}. Set {} in the agent's env from a trusted source.",
        match unset.as_slice() {
            [one] => format!("{one} is"),
            [first, second] => format!("{first} and {second} are"),
            [rest @ .., last] => format!("{} and {last} are", rest.join(", ")),
            [] => String::new(),
        },
        refused.join(", and "),
        if unset.len() == 1 { "it" } else { "them" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party(hint: &str, digit: char) -> String {
        format!("{hint}::1220{}", digit.to_string().repeat(64))
    }

    fn env_file(tag: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pins-{tag}-{}",
            agent_logic::clock::now_micros_i64()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(".env");
        std::fs::write(&file, content).unwrap();
        file
    }

    #[test]
    fn a_template_names_its_pins_as_party_ids_or_is_refused() {
        let (fee, traffic) = (party("fee", 'a'), party("traffic", 'b'));
        let env = format!(
            "SYNCHRONIZER_ID=s\n# PARTY_FEE=x\nPARTY_FEE=\"{fee}\"\nPARTY_PREPAID_TRAFFIC={traffic}\n"
        );
        assert_eq!(
            template_pins(&env).unwrap(),
            vec![
                ("PARTY_FEE", fee.clone()),
                ("PARTY_PREPAID_TRAFFIC", traffic.clone())
            ]
        );
        assert_eq!(
            template_pins("PARTY_FEE=\nNODE_NAME=n\n").unwrap(),
            Vec::new()
        );
        for bad in ["fee", "fee::1220zz", "fee party::1220aa"] {
            let err = template_pins(&format!("PARTY_FEE={bad}\n"))
                .unwrap_err()
                .to_string();
            assert!(err.contains("refusing to pin PARTY_FEE"), "{err}");
        }
    }

    #[test]
    fn a_pin_is_written_once_and_a_local_value_is_never_replaced() {
        let (fee, traffic, mine) = (party("fee", 'a'), party("traffic", 'b'), party("mine", 'c'));
        let file = env_file("once", "PARTY_AGENT=a::1220\nPARTY_FEE=\n");
        let pins = vec![
            ("PARTY_FEE", fee.clone()),
            ("PARTY_PREPAID_TRAFFIC", traffic.clone()),
        ];
        let states = pin_missing(&file, &pins, &|_| None).unwrap();
        assert_eq!(
            states,
            vec![
                ("PARTY_FEE", PinState::Pinned(fee.clone())),
                ("PARTY_PREPAID_TRAFFIC", PinState::Pinned(traffic.clone()))
            ]
        );
        assert_eq!(
            crate::read_env_value(&file, "PARTY_FEE").as_deref(),
            Some(fee.as_str())
        );
        // Another value later is never written over the pin.
        let other = vec![("PARTY_FEE", mine.clone())];
        let states = pin_missing(&file, &other, &|_| None).unwrap();
        assert_eq!(
            states.first(),
            Some(&(
                "PARTY_FEE",
                PinState::Kept {
                    local: fee.clone(),
                    offered: Some(mine.clone())
                }
            ))
        );
        assert_eq!(
            crate::read_env_value(&file, "PARTY_FEE").as_deref(),
            Some(fee.as_str())
        );
        assert_eq!(
            crate::read_env_value(&file, "PARTY_PREPAID_TRAFFIC").as_deref(),
            Some(traffic.as_str()),
            "never removed"
        );
        // A value set in the process counts as pinned, and the file is left alone.
        let fresh = env_file("process", "PARTY_AGENT=a::1220\n");
        let states = pin_missing(&fresh, &pins, &|name| {
            (name == "PARTY_FEE").then(|| mine.clone().into())
        })
        .unwrap();
        assert_eq!(
            states.first(),
            Some(&(
                "PARTY_FEE",
                PinState::Kept {
                    local: mine.clone(),
                    offered: Some(fee.clone())
                }
            ))
        );
        assert_eq!(crate::read_env_value(&fresh, "PARTY_FEE"), None);
        assert_eq!(missing(&fresh, &|_| None), vec!["PARTY_FEE", "PARTY_TRAFFIC_FEE"]);
        let mut out = Vec::new();
        write_states(&mut out, &[("PARTY_FEE", PinState::Unset)]);
        assert!(
            String::from_utf8_lossy(&out)
                .contains("PARTY_FEE is not set: a linked agent refuses to sign its fee payments")
        );
    }

    #[test]
    fn a_linked_agent_without_a_pin_is_warned_once_with_what_is_refused() {
        let (fee, traffic) = (party("fee", 'a'), party("traffic", 'b'));
        let mut config = BaseConfig::test_minimal().unwrap();
        config.party_fee = None;
        config.party_prepaid_traffic = None;
        let both = unset_warning(&config).unwrap();
        assert!(
            both.starts_with("PARTY_FEE and PARTY_PREPAID_TRAFFIC are not pinned"),
            "{both}"
        );
        assert!(
            both.contains("fee payments") && both.contains("prepaid traffic top-ups"),
            "{both}"
        );
        config.party_fee = Some(fee.clone());
        let only = unset_warning(&config).unwrap();
        assert!(
            only.starts_with("PARTY_PREPAID_TRAFFIC is not pinned")
                && !only.contains("fee payments"),
            "{only}"
        );
        config.party_prepaid_traffic = Some(traffic.clone());
        assert_eq!(unset_warning(&config), None);
    }

    #[test]
    fn the_traffic_fee_pin_is_taken_once_and_reported_only_where_it_is_named() {
        let (fee, traffic, traffic_fee, mine) =
            (party("fee", 'a'), party("traffic", 'b'), party("traffic-fee", 'd'), party("mine", 'c'));
        let env = format!("PARTY_FEE={fee}\nPARTY_PREPAID_TRAFFIC={traffic}\nPARTY_TRAFFIC_FEE={traffic_fee}\n");
        let named = template_pins(&env).unwrap();
        assert_eq!(named.last(), Some(&("PARTY_TRAFFIC_FEE", traffic_fee.clone())));
        let err = template_pins("PARTY_TRAFFIC_FEE=traffic-fee\n").unwrap_err().to_string();
        assert!(err.contains("refusing to pin PARTY_TRAFFIC_FEE"), "{err}");
        // Written where unset, never replaced.
        let file = env_file("traffic-fee", "PARTY_AGENT=a::1220\n");
        let states = pin_missing(&file, &named, &|_| None).unwrap();
        assert_eq!(states.last(), Some(&("PARTY_TRAFFIC_FEE", PinState::Pinned(traffic_fee.clone()))));
        let other = vec![("PARTY_TRAFFIC_FEE", mine.clone())];
        let states = pin_missing(&file, &other, &|_| None).unwrap();
        assert_eq!(
            states.last(),
            Some(&("PARTY_TRAFFIC_FEE", PinState::Kept { local: traffic_fee.clone(), offered: Some(mine.clone()) }))
        );
        assert_eq!(crate::read_env_value(&file, "PARTY_TRAFFIC_FEE").as_deref(), Some(traffic_fee.as_str()));
        // Named nowhere and set nowhere, it is not reported: only token top-ups need it.
        let bare = env_file("traffic-fee-bare", "PARTY_AGENT=a::1220\n");
        let states = pin_missing(&bare, &named[..2], &|_| None).unwrap();
        assert_eq!(states.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec!["PARTY_FEE", "PARTY_PREPAID_TRAFFIC"]);
        let mut out = Vec::new();
        write_states(&mut out, &[("PARTY_TRAFFIC_FEE", PinState::Unset)]);
        assert!(
            String::from_utf8_lossy(&out).contains("PARTY_TRAFFIC_FEE is not set: a linked agent refuses to sign its prepaid traffic top-ups paid in a token other than CC"),
            "{}",
            String::from_utf8_lossy(&out)
        );
        // The content check takes it from the env file, once.
        let filled = fill_unset(&file, &PlatformParties::default(), false);
        assert_eq!(filled.traffic_fee.as_deref(), Some(traffic_fee.as_str()));
        let kept = PlatformParties { traffic_fee: Some(mine.clone()), ..PlatformParties::default() };
        assert_eq!(fill_unset(&file, &kept, false).traffic_fee.as_deref(), Some(mine.as_str()));
        let malformed = env_file("traffic-fee-bad", "PARTY_TRAFFIC_FEE=traffic-fee\n");
        assert_eq!(fill_unset(&malformed, &PlatformParties::default(), true).traffic_fee, None);
    }

    #[test]
    fn the_traffic_fee_pin_is_warned_about_only_when_a_token_top_up_is_listed() {
        let (fee, traffic, traffic_fee) = (party("fee", 'a'), party("traffic", 'b'), party("traffic-fee", 'd'));
        let tokens = |list: &[&str]| list.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        let two = PlatformParties { fee: Some(fee.clone()), prepaid_traffic: Some(traffic.clone()), traffic_fee: None };
        assert_eq!(unset_warning_of(&two, &tokens(&["CC"])), None);
        assert_eq!(unset_warning_of(&two, &tokens(&[])), None);
        let warned = unset_warning_of(&two, &tokens(&["TUSDT", "CC"])).unwrap();
        assert!(warned.starts_with("PARTY_TRAFFIC_FEE is not pinned"), "{warned}");
        assert!(warned.contains("paid in a token other than CC"), "{warned}");
        assert!(!traffic_fee_needed(&tokens(&["CC", "cc"])));
        assert!(traffic_fee_needed(&tokens(&["USDCx"])));
        let none = PlatformParties::default();
        let all = unset_warning_of(&none, &tokens(&["USDCx"])).unwrap();
        assert!(all.starts_with("PARTY_FEE, PARTY_PREPAID_TRAFFIC and PARTY_TRAFFIC_FEE are not pinned"), "{all}");
        let three = PlatformParties { traffic_fee: Some(traffic_fee), ..two };
        assert_eq!(unset_warning_of(&three, &tokens(&["USDCx"])), None);
        let mut config = BaseConfig::test_minimal().unwrap();
        config.party_fee = Some(fee);
        config.party_prepaid_traffic = Some(traffic);
        assert_eq!(unset_warning(&config), None);
        config.prepaid_traffic_topup_tokens = tokens(&["TUSDT"]);
        assert!(unset_warning(&config).is_some_and(|w| w.starts_with("PARTY_TRAFFIC_FEE is not pinned")));
    }

    #[test]
    fn the_server_config_never_writes_the_traffic_fee_pin_over_a_local_one() {
        let (served_party, mine) = (party("served", 'e'), party("mine", 'f'));
        let file = env_file("server-config", &format!("PARTY_AGENT=a::1220\nPARTY_TRAFFIC_FEE={mine}\n"));
        let served = orderbook_proto::ledger::GetAgentConfigResponse {
            env: format!("SYNCHRONIZER_ID=s\nPARTY_TRAFFIC_FEE={served_party}\n"),
            ..Default::default()
        };
        let states = crate::write_server_config_with(&file, "http://rpc", &served, &|_| None).unwrap();
        assert_eq!(
            states.last(),
            Some(&("PARTY_TRAFFIC_FEE", PinState::Kept { local: mine.clone(), offered: Some(served_party) }))
        );
        assert_eq!(crate::read_env_value(&file, "PARTY_TRAFFIC_FEE").as_deref(), Some(mine.as_str()));
        assert_eq!(crate::read_env_value(&file, "SYNCHRONIZER_ID").as_deref(), Some("s"));
    }
}
