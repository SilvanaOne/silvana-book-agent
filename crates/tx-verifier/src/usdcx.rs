//! USDCx bridge (Circle xReserve through Digital Asset's `utility-bridge-v0`):
//! the pinned parties per network, the bridge packages a transaction may use
//! and the deterministic command ids of the typed ledger-service operations.
//!
//! The values are pinned on purpose: nothing a server returns is used as the
//! source of a party, a synchronizer or a package.

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

/// Instrument id of USDCx.
pub const INSTRUMENT_ID: &str = "USDCx";
/// Holding scheme of USDCx (the instrument configuration's default identifier).
pub const HOLDING_SCHEME: &str = "RegistrarInternalScheme";
/// Circle domain id of Ethereum: the only burn destination.
pub const ETHEREUM_DOMAIN: i64 = 0;

/// `utility-bridge-v0` packages a USDCx transaction may use. A participant
/// prepares with a version that every participant hosting the involved
/// parties vets, so DA's own hosting decides which one appears.
pub const BRIDGE_PACKAGE_IDS: [&str; 2] = [
    // 0.1.5
    "1eace3edb43892eb380a4b5825d94367b0f01705a9eae43b0a7386d0c5d59abb",
    // 0.1.6
    "3a2ef1ece110eccc776a9605f68513ef3924ccbcf245f8927cb566c7394ede75",
];

/// Templates and choices of `utility-bridge-v0` (module, entity).
pub const T_AGREEMENT_REQUEST: (&str, &str) = (
    "Utility.Bridge.V0.Agreement.User",
    "BridgeUserAgreementRequest",
);
pub const T_AGREEMENT: (&str, &str) = ("Utility.Bridge.V0.Agreement.User", "BridgeUserAgreement");
pub const T_DEPOSIT_ATTESTATION: (&str, &str) = (
    "Utility.Bridge.V0.Attestation.Deposit",
    "DepositAttestation",
);
pub const T_WITHDRAW_ATTESTATION: (&str, &str) = (
    "Utility.Bridge.V0.Attestation.Withdraw",
    "WithdrawAttestation",
);
pub const T_ACCEPTED_WITHDRAW: (&str, &str) = (
    "Utility.Bridge.V0.Attestation.Withdraw",
    "AcceptedWithdrawAttestation",
);
pub const T_REVOKED_WITHDRAW: (&str, &str) = (
    "Utility.Bridge.V0.Attestation.Withdraw",
    "RevokedWithdrawAttestation",
);
/// Registry templates the burn and mint choices use (module, entity).
pub const T_HOLDING: (&str, &str) = ("Utility.Registry.Holding.V0.Holding", "Holding");
pub const T_ALLOCATION_FACTORY: (&str, &str) = (
    "Utility.Registry.App.V0.Service.AllocationFactory",
    "AllocationFactory",
);
pub const T_INSTRUMENT_CONFIGURATION: (&str, &str) = (
    "Utility.Registry.V0.Configuration.Instrument",
    "InstrumentConfiguration",
);

pub const CHOICE_MINT: &str = "BridgeUserAgreement_Mint";
pub const CHOICE_BURN: &str = "BridgeUserAgreement_Burn";
pub const CHOICE_RECLAIM: &str = "BridgeUserAgreement_ReclaimHolding";
pub const CHOICE_UPDATE_PRE_APPROVAL: &str = "BridgeUserAgreement_UpdatePreApproval";
pub const CHOICE_TERMINATE: &str = "BridgeUserAgreement_Terminate";

/// The parties and synchronizer of the bridge on one network.
#[derive(Debug)]
pub struct UsdcxNetwork {
    pub name: &'static str,
    pub synchronizer_id: &'static str,
    /// USDCx instrument admin: the registrar and `crossChainRepresentative`.
    pub admin: &'static str,
    pub bridge_operator: &'static str,
    /// DA's utility operator (`operator` on the bridge contracts).
    pub utility_operator: &'static str,
}

pub static MAINNET: UsdcxNetwork = UsdcxNetwork {
    name: "mainnet",
    synchronizer_id: "global-domain::1220b1431ef217342db44d516bb9befde802be7d8899637d290895fa58880f19accc",
    admin: "decentralized-usdc-interchain-rep::12208115f1e168dd7e792320be9c4ca720c751a02a3053c7606e1c1cd3dad9bf60ef",
    bridge_operator: "Bridge-Operator::1220c8448890a70e65f6906bd48d797ee6551f094e9e6a53e329fd5b2b549334f13f",
    utility_operator: "auth0_007c6643538f2eadd3e573dd05b9::12205bcc106efa0eaa7f18dc491e5c6f5fb9b0cc68dc110ae66f4ed6467475d7c78e",
};

pub static TESTNET: UsdcxNetwork = UsdcxNetwork {
    name: "testnet",
    synchronizer_id: "global-domain::1220f22a8b8f2d813c25b9a684dc4dd52b532a0174d8e73a13cdf2baabfff7518337",
    admin: "decentralized-usdc-interchain-rep::122049e2af8a725bd19759320fc83c638e7718973eac189d8f201309c512d1ffec61",
    bridge_operator: "Bridge-Operator::12209d011ce250de439fefc35d16d1ab9d56fb99ccb24c18d798efb22352d533bcdb",
    utility_operator: "DigitalAsset-UtilityOperator::12202679f2bbe57d8cba9ef3cee847ac8239df0877105ab1f01a77d47477fdce1204",
};

impl UsdcxNetwork {
    /// `mainnet` or `testnet`. USDCx is not available elsewhere.
    pub fn by_name(name: &str) -> Result<&'static UsdcxNetwork> {
        match name.trim().to_ascii_lowercase().as_str() {
            "mainnet" => Ok(&MAINNET),
            "testnet" => Ok(&TESTNET),
            other => bail!("USDCx is not available on network {other:?} (mainnet or testnet only)"),
        }
    }

    /// The network of a synchronizer.
    pub fn by_synchronizer(synchronizer_id: &str) -> Result<&'static UsdcxNetwork> {
        [&MAINNET, &TESTNET]
            .into_iter()
            .find(|n| n.synchronizer_id == synchronizer_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "USDCx is not available on synchronizer {synchronizer_id} \
                     (MainNet and TestNet only)"
                )
            })
    }
}

/// Characters of a SHA-256 hex digest kept in a command id.
const COMMAND_ID_HASH_HEX_LEN: usize = 40;

/// Deterministic command id `usdcx-<op>-<first 40 hex characters of
/// sha256(key)>` of a typed USDCx operation. The participant deduplicates on
/// it, so a retry of the same operation cannot commit twice while the
/// deduplication period lasts.
pub fn command_id(op: &str, key: &str) -> String {
    let hash: String = Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        .chars()
        .take(COMMAND_ID_HASH_HEX_LEN)
        .collect();
    format!("usdcx-{op}-{hash}")
}

/// The burn of withdrawal `request_id`.
pub fn burn_command_id(request_id: &str) -> String {
    command_id("burn", request_id)
}

/// The mint of a DepositAttestation.
pub fn mint_command_id(deposit_attestation_cid: &str) -> String {
    command_id("mint", deposit_attestation_cid)
}

/// The reclaim of a RevokedWithdrawAttestation.
pub fn reclaim_command_id(revoked_withdraw_attestation_cid: &str) -> String {
    command_id("reclaim", revoked_withdraw_attestation_cid)
}

/// The onboarding request `request_id` of `party`.
pub fn onboard_command_id(party: &str, request_id: &str) -> String {
    command_id("onboard", &format!("{party}|{request_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks_resolve_by_name_and_synchronizer() {
        assert_eq!(UsdcxNetwork::by_name(" MainNet ").unwrap().name, "mainnet");
        assert_eq!(UsdcxNetwork::by_name("testnet").unwrap().name, "testnet");
        assert!(UsdcxNetwork::by_name("devnet").is_err());
        assert_eq!(
            UsdcxNetwork::by_synchronizer(MAINNET.synchronizer_id)
                .unwrap()
                .name,
            "mainnet"
        );
        assert!(UsdcxNetwork::by_synchronizer("global-domain::1220aa").is_err());
    }

    /// Shared with the server's builders: both sides must derive the same ids.
    #[test]
    fn command_ids_are_stable() {
        assert_eq!(
            burn_command_id("6f1c2c7e-4b8a-4c41-9a3e-2d5b7f0c9e11"),
            command_id("burn", "6f1c2c7e-4b8a-4c41-9a3e-2d5b7f0c9e11")
        );
        let id = burn_command_id("6f1c2c7e-4b8a-4c41-9a3e-2d5b7f0c9e11");
        assert!(id.starts_with("usdcx-burn-"));
        assert_eq!(id.len(), "usdcx-burn-".len() + 40);
        assert_eq!(
            command_id("burn", "abc"),
            // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
            "usdcx-burn-ba7816bf8f01cfea414140de5dae2223b00361a3"
        );
        assert_eq!(
            onboard_command_id("p::1220", "r"),
            command_id("onboard", "p::1220|r")
        );
        assert_ne!(mint_command_id("00a"), reclaim_command_id("00a"));
    }
}
