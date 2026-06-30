//! Auth Agent — JWT generation/inspection CLI
//!
//! Thin wrapper around the existing `auth.rs` helpers. Generates the
//! self-describing Ed25519 JWTs (RFC 8037) used by Silvana orderbook services
//! and decodes/verifies tokens for debugging.
//!
//! Reads the private key from `PARTY_AGENT_PRIVATE_KEY` env (Base58, Solana-style)
//! or `--private-key`. The party id is taken from `PARTY_AGENT` env or `--party`.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};

use agent_logic::auth::{
    decode_base58_private_key, extract_user_id_from_jwt, generate_jwt, get_public_key_hex,
};

#[derive(Parser)]
#[command(name = "agent-auth")]
#[command(about = "JWT generation and inspection for the Silvana orderbook")]
struct Cli {
    #[arg(long, global = true, env = "PARTY_AGENT_PRIVATE_KEY", hide_env_values = true)]
    private_key: Option<String>,

    #[arg(long, global = true, env = "PARTY_AGENT")]
    party: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the public key derived from the configured private key
    PublicKey,
    /// Generate a self-describing Ed25519 JWT for the configured party
    Generate {
        #[arg(long, default_value = "trader")]
        role: String,

        #[arg(long, default_value = "3600")]
        ttl_secs: u64,

        /// Optional Canton node_name claim
        #[arg(long, env = "NODE_NAME")]
        node_name: Option<String>,

        /// Print only the bare token (no labels) — useful for `agent-auth generate | xargs curl ...`
        #[arg(long)]
        bare: bool,
    },
    /// Decode a JWT and print header + claims as pretty JSON (no signature verification)
    Decode {
        #[arg(long)]
        jwt: String,
    },
    /// Verify a JWT signature using the embedded `jwk` public key (RFC 8037)
    Verify {
        #[arg(long)]
        jwt: String,
    },
    /// Print the `sub` (party_id) claim from a JWT
    UserId {
        #[arg(long)]
        jwt: String,
    },
}

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    match cli.command {
        Commands::PublicKey => {
            let key = require_private_key(cli.private_key.as_deref())?;
            println!("hex: {}", get_public_key_hex(&key));
            let pk = SigningKey::from_bytes(&key).verifying_key();
            println!("base58: {}", bs58::encode(pk.as_bytes()).into_string());
            println!("base64url: {}", URL_SAFE_NO_PAD.encode(pk.as_bytes()));
            Ok(())
        }
        Commands::Generate {
            role,
            ttl_secs,
            node_name,
            bare,
        } => {
            let key = require_private_key(cli.private_key.as_deref())?;
            let party = cli
                .party
                .clone()
                .context("--party not provided and PARTY_AGENT env not set")?;
            let jwt = generate_jwt(&party, &role, &key, ttl_secs, node_name.as_deref())?;
            if bare {
                println!("{}", jwt);
            } else {
                let exp = chrono::Utc::now() + chrono::Duration::seconds(ttl_secs as i64);
                println!("party: {}", party);
                println!("role:  {}", role);
                println!("ttl:   {}s (expires {})", ttl_secs, exp.to_rfc3339());
                println!("jwt:   {}", jwt);
            }
            Ok(())
        }
        Commands::Decode { jwt } => {
            let (header, claims) = split_jwt(&jwt)?;
            let header_json: serde_json::Value = serde_json::from_slice(&header)
                .context("Failed to parse JWT header as JSON")?;
            let claims_json: serde_json::Value = serde_json::from_slice(&claims)
                .context("Failed to parse JWT claims as JSON")?;
            println!("=== header ===");
            println!("{}", serde_json::to_string_pretty(&header_json)?);
            println!("\n=== claims ===");
            println!("{}", serde_json::to_string_pretty(&claims_json)?);
            if let Some(exp) = claims_json.get("exp").and_then(|v| v.as_i64()) {
                let dt: Option<DateTime<Utc>> = Utc.timestamp_opt(exp, 0).single();
                if let Some(dt) = dt {
                    println!("\nexp interpreted: {}", dt.to_rfc3339());
                }
            }
            Ok(())
        }
        Commands::Verify { jwt } => {
            verify_jwt(&jwt)?;
            println!("OK");
            Ok(())
        }
        Commands::UserId { jwt } => {
            let sub = extract_user_id_from_jwt(&jwt)?;
            println!("{}", sub);
            Ok(())
        }
    }
}

fn require_private_key(flag: Option<&str>) -> Result<[u8; 32]> {
    let raw = flag
        .map(|s| s.to_string())
        .or_else(|| std::env::var("PARTY_AGENT_PRIVATE_KEY").ok())
        .context("--private-key not provided and PARTY_AGENT_PRIVATE_KEY env not set")?;
    decode_base58_private_key(&raw)
}

fn split_jwt(jwt: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err(anyhow!("Invalid JWT: expected 3 dot-separated parts, got {}", parts.len()));
    }
    let header = URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|e| anyhow!("Invalid JWT header b64: {}", e))?;
    let claims = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| anyhow!("Invalid JWT claims b64: {}", e))?;
    Ok((header, claims))
}

fn verify_jwt(jwt: &str) -> Result<()> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err(anyhow!("Invalid JWT: expected 3 parts"));
    }
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let (header_bytes, _claims_bytes) = split_jwt(jwt)?;
    let header: serde_json::Value = serde_json::from_slice(&header_bytes)
        .context("Failed to parse JWT header")?;

    let jwk_x = header
        .get("jwk")
        .and_then(|j| j.get("x"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("JWT header has no jwk.x (this verifier only handles RFC 8037 self-describing tokens)"))?;

    let pk_bytes = URL_SAFE_NO_PAD
        .decode(jwk_x)
        .map_err(|e| anyhow!("Invalid jwk.x base64url: {}", e))?;
    let pk_arr: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| anyhow!("jwk.x must be 32 bytes"))?;
    let verifying_key = VerifyingKey::from_bytes(&pk_arr)
        .map_err(|e| anyhow!("Invalid Ed25519 public key: {}", e))?;

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|e| anyhow!("Invalid signature b64url: {}", e))?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow!("JWT signature must be 64 bytes"))?;
    let signature = Signature::from_bytes(&sig_arr);

    verifying_key
        .verify(signing_input.as_bytes(), &signature)
        .map_err(|_| anyhow!("JWT signature verification FAILED"))
}
