//! The platform parties an agent pins on first use, the fee party and the prepaid traffic
//! party: written only where the agent's env sets none, never replaced or removed.

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

/// The env keys of the two platform parties.
pub const PIN_KEYS: [&str; 2] = ["PARTY_FEE", "PARTY_PREPAID_TRAFFIC"];
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
            (None, None) => PinState::Unset,
        };
        states.push((key, state));
    }
    Ok(states)
}

/// What a linked agent refuses while `key` is unset.
fn refused_payments(key: &str) -> &'static str {
    if key == "PARTY_FEE" {
        "to sign its fee payments and to take a swap quote with a fee"
    } else {
        "to sign its prepaid traffic top-ups"
    }
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

/// The keys a link would pin, each with the value the agent config names; empty when the
/// env sets both. A failed read only leaves the values unknown.
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
                "The platform parties were not written to {} ({e:#}); set PARTY_FEE and PARTY_PREPAID_TRAFFIC there by hand.",
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

/// The startup warning of a linked agent whose env lacks a pin; None with both set.
pub fn unset_warning(config: &BaseConfig) -> Option<String> {
    unset_warning_of(&PlatformParties::from_config(config))
}

/// [`unset_warning`] for the pins in force.
pub fn unset_warning_of(parties: &PlatformParties) -> Option<String> {
    let unset: Vec<&str> = [
        ("PARTY_FEE", &parties.fee),
        ("PARTY_PREPAID_TRAFFIC", &parties.prepaid_traffic),
    ]
    .into_iter()
    .filter(|(_, value)| value.is_none())
    .map(|(key, _)| key)
    .collect();
    if unset.is_empty() {
        return None;
    }
    let refused: Vec<&str> = unset.iter().map(|key| refused_payments(key)).collect();
    Some(format!(
        "{} not pinned: this linked agent refuses {}. Set {} in the agent's env from a trusted source.",
        match unset.as_slice() {
            [one] => format!("{one} is"),
            _ => format!("{} are", unset.join(" and ")),
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
        assert_eq!(missing(&fresh, &|_| None), vec!["PARTY_FEE"]);
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
}
