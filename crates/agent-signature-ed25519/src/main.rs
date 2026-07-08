//! Signature Agent — off-chain Ed25519 signing CLI
//!
//! Thin wrapper around the existing `message-signing` crate and `auth.rs`
//! helpers. Performs Ed25519 sign/verify and the canonical
//! `ed25519-sha256-v1` scheme used by the Ledger gRPC service.
//!
//! Reads the private key from `PARTY_AGENT_PRIVATE_KEY` (Base58, Solana-style)
//! or `--private-key` flag. The key never leaves this process.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD};
use base64::Engine;
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use agent_logic::auth::decode_base58_private_key;

#[derive(Parser)]
#[command(name = "agent-signature-ed25519")]
#[command(about = "Off-chain Ed25519 signing and verification")]
struct Cli {
    /// Base58-encoded Ed25519 private key. Falls back to PARTY_AGENT_PRIVATE_KEY env.
    #[arg(long, global = true, env = "PARTY_AGENT_PRIVATE_KEY", hide_env_values = true)]
    private_key: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print this party's Ed25519 public key in multiple formats
    PublicKey,
    /// Sign arbitrary raw bytes with Ed25519 (no hashing — caller chooses message)
    SignRaw {
        /// Plain UTF-8 input (mutually exclusive with --input-base64)
        #[arg(long, conflicts_with = "input_base64")]
        input: Option<String>,
        /// Base64-encoded binary input
        #[arg(long, conflicts_with = "input")]
        input_base64: Option<String>,
    },
    /// Sign using the canonical `ed25519-sha256-v1` scheme (SHA-256 then sign)
    ///
    /// This is the scheme used by the Ledger gRPC service for request/response signatures.
    SignCanonical {
        #[arg(long, conflicts_with = "input_base64")]
        input: Option<String>,
        #[arg(long, conflicts_with = "input")]
        input_base64: Option<String>,
    },
    /// Verify a raw Ed25519 signature
    VerifyRaw {
        #[arg(long, conflicts_with = "input_base64")]
        input: Option<String>,
        #[arg(long, conflicts_with = "input")]
        input_base64: Option<String>,
        /// Base64-encoded 64-byte Ed25519 signature
        #[arg(long)]
        signature: String,
        /// Base64url-encoded 32-byte Ed25519 public key (RFC 8037 `x`)
        #[arg(long)]
        public_key: String,
    },
    /// Verify a canonical (`ed25519-sha256-v1`) signature
    VerifyCanonical {
        #[arg(long, conflicts_with = "input_base64")]
        input: Option<String>,
        #[arg(long, conflicts_with = "input")]
        input_base64: Option<String>,
        #[arg(long)]
        signature: String,
        #[arg(long)]
        public_key: String,
    },
}

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    match cli.command {
        Commands::PublicKey => {
            let key = load_private_key(cli.private_key)?;
            let pk = SigningKey::from_bytes(&key).verifying_key();
            let bytes = pk.to_bytes();
            println!("hex:            {}", hex::encode(bytes));
            println!("base58:         {}", bs58::encode(bytes).into_string());
            println!("base64url (jwk x): {}", URL_SAFE_NO_PAD.encode(bytes));
            Ok(())
        }
        Commands::SignRaw { input, input_base64 } => {
            let key = load_private_key(cli.private_key)?;
            let data = decode_input(input, input_base64)?;
            let signing_key = SigningKey::from_bytes(&key);
            let signature = signing_key.sign(&data);
            println!("signature_b64: {}", BASE64.encode(signature.to_bytes()));
            println!(
                "public_key_b64url: {}",
                URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes())
            );
            Ok(())
        }
        Commands::SignCanonical { input, input_base64 } => {
            let key = load_private_key(cli.private_key)?;
            let data = decode_input(input, input_base64)?;
            let result = message_signing::sign_canonical(&key, &data);
            println!("signing_scheme:    {}", result.signing_scheme);
            println!("signature_b64:     {}", result.signature_b64);
            println!("public_key_b64url: {}", result.public_key_b64url);
            Ok(())
        }
        Commands::VerifyRaw {
            input,
            input_base64,
            signature,
            public_key,
        } => {
            let data = decode_input(input, input_base64)?;
            let pk_bytes = message_signing::parse_public_key(&public_key)?;
            let vk = VerifyingKey::from_bytes(&pk_bytes)
                .map_err(|e| anyhow!("Invalid public key: {}", e))?;
            let sig_bytes = BASE64
                .decode(&signature)
                .map_err(|e| anyhow!("Invalid signature base64: {}", e))?;
            let sig_arr: [u8; 64] = sig_bytes
                .try_into()
                .map_err(|_| anyhow!("Signature must be 64 bytes"))?;
            let sig = Signature::from_bytes(&sig_arr);
            match vk.verify(&data, &sig) {
                Ok(()) => {
                    println!("OK");
                    Ok(())
                }
                Err(_) => Err(anyhow!("signature verification FAILED")),
            }
        }
        Commands::VerifyCanonical {
            input,
            input_base64,
            signature,
            public_key,
        } => {
            let data = decode_input(input, input_base64)?;
            let pk_bytes = message_signing::parse_public_key(&public_key)?;
            message_signing::verify_canonical(
                &pk_bytes,
                &data,
                &signature,
                message_signing::SIGNING_SCHEME,
            )?;
            println!("OK");
            Ok(())
        }
    }
}

fn load_private_key(flag: Option<String>) -> Result<[u8; 32]> {
    let raw = flag
        .or_else(|| std::env::var("PARTY_AGENT_PRIVATE_KEY").ok())
        .context("--private-key not provided and PARTY_AGENT_PRIVATE_KEY env not set")?;
    decode_base58_private_key(&raw)
}

fn decode_input(plain: Option<String>, b64: Option<String>) -> Result<Vec<u8>> {
    match (plain, b64) {
        (Some(s), None) => Ok(s.into_bytes()),
        (None, Some(b)) => BASE64
            .decode(b)
            .map_err(|e| anyhow!("Invalid --input-base64: {}", e)),
        (None, None) => Err(anyhow!("Provide --input or --input-base64")),
        (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
    }
}
