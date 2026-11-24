//! Self-describing JWTs: Ed25519, with the public key as the header `jwk` (RFC 8037).

use agent_control::{CONTROL_TOKEN_AUDIENCE_PREFIX, HeadRef, Network};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The claims of one token.
#[derive(Clone, Copy, Debug)]
pub struct JwtRequest<'a> {
    pub sub: &'a str,
    pub role: &'a str,
    pub ttl_secs: u64,
    pub node_name: Option<&'a str>,
    /// RFQ V2 venue branch; empty is the same as none.
    pub venue_branch: Option<&'a str>,
}

/// Seconds since the Unix epoch.
pub(crate) fn now_secs() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("System time error")?
        .as_secs())
}

/// Longest lifetime of a token for the control services.
pub const CONTROL_TOKEN_MAX_TTL_SECS: u64 = 900;

/// The lifetime of a control-service token: `token_ttl_secs`, at most 900 s.
pub fn control_ttl_secs(token_ttl_secs: u64) -> u64 {
    token_ttl_secs.min(CONTROL_TOKEN_MAX_TTL_SECS)
}

/// The unsigned `header.claims` of a token for `public_key`, issued at `now`.
#[cfg(test)]
pub(crate) fn signing_input(
    public_key: &[u8; 32],
    req: &JwtRequest<'_>,
    now: u64,
) -> Result<String> {
    signing_input_with_head(public_key, req, now, None)
}

/// The `aud` of a control-service token on `network`.
pub fn control_audience(network: Network) -> String {
    format!("{CONTROL_TOKEN_AUDIENCE_PREFIX}{}", network.as_str())
}

/// [`signing_input`] carrying `ctl_seq` (integer) and `ctl_head` (hex) when `head` is set.
#[cfg(test)]
pub(crate) fn signing_input_with_head(
    public_key: &[u8; 32],
    req: &JwtRequest<'_>,
    now: u64,
    head: Option<&HeadRef>,
) -> Result<String> {
    signing_input_for(public_key, req, now, head, None)
}

/// [`signing_input_with_head`] also carrying `aud` when `audience` is set.
pub(crate) fn signing_input_for(
    public_key: &[u8; 32],
    req: &JwtRequest<'_>,
    now: u64,
    head: Option<&HeadRef>,
    audience: Option<&str>,
) -> Result<String> {
    let mut claims = serde_json::json!({
        "sub": req.sub,
        "exp": now.saturating_add(req.ttl_secs),
        "iat": now,
        "role": req.role,
    });
    if let Some(obj) = claims.as_object_mut() {
        if let Some(nn) = req.node_name {
            obj.insert(
                "node_name".to_string(),
                serde_json::Value::String(nn.to_string()),
            );
        }
        if let Some(branch) = req.venue_branch.filter(|b| !b.is_empty()) {
            obj.insert(
                "venue_branch".to_string(),
                serde_json::Value::String(branch.to_string()),
            );
        }
        if let Some(head) = head {
            obj.insert("ctl_seq".to_string(), serde_json::Value::from(head.seq));
            obj.insert(
                "ctl_head".to_string(),
                serde_json::Value::String(head.hash.to_string()),
            );
        }
        if let Some(audience) = audience {
            obj.insert(
                "aud".to_string(),
                serde_json::Value::String(audience.to_string()),
            );
        }
    }
    let header = serde_json::json!({
        "typ": "JWT",
        "alg": "EdDSA",
        "jwk": {
            "kty": "OKP",
            "crv": "Ed25519",
            "x": URL_SAFE_NO_PAD.encode(public_key)
        }
    });
    let header_json = serde_json::to_string(&header).context("Failed to serialize header")?;
    let claims_json = serde_json::to_string(&claims).context("Failed to serialize claims")?;
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header_json.as_bytes()),
        URL_SAFE_NO_PAD.encode(claims_json.as_bytes())
    ))
}

fn decode_segment(segment: &[u8]) -> Result<serde_json::Value> {
    let raw = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| anyhow!("signing refused: not a JWT"))?;
    serde_json::from_slice(&raw).map_err(|_| anyhow!("signing refused: not a JWT"))
}

/// A signing input for `public_key`, naming `sub`, claiming exactly `head`, for the
/// control services of `network` and valid at most [`CONTROL_TOKEN_MAX_TTL_SECS`].
pub(crate) fn check_head_token(
    input: &[u8],
    public_key: &[u8; 32],
    sub: &str,
    head: &HeadRef,
    network: Network,
) -> Result<()> {
    let mut parts = input.split(|b| *b == b'.');
    let (Some(header), Some(claims), None) = (parts.next(), parts.next(), parts.next()) else {
        bail!("signing refused: not a JWT");
    };
    let header = decode_segment(header)?;
    let x = URL_SAFE_NO_PAD.encode(public_key);
    ensure!(
        header.get("alg").and_then(|v| v.as_str()) == Some("EdDSA")
            && header.get("typ").and_then(|v| v.as_str()) == Some("JWT")
            && header.pointer("/jwk/x").and_then(|v| v.as_str()) == Some(x.as_str()),
        "signing refused: not a JWT for this key"
    );
    let claims = decode_segment(claims)?;
    ensure!(
        claims.get("sub").and_then(|v| v.as_str()) == Some(sub),
        "signing refused: a head token names only the agent's own party"
    );
    ensure!(
        claims.get("ctl_seq").and_then(|v| v.as_u64()) == Some(head.seq)
            && claims.get("ctl_head").and_then(|v| v.as_str())
                == Some(head.hash.to_string().as_str()),
        "signing refused: the token does not claim the held head"
    );
    ensure!(
        claims.get("aud").and_then(|v| v.as_str()) == Some(control_audience(network).as_str()),
        "signing refused: a head token is only for the control services of {network}"
    );
    let iat = claims.get("iat").and_then(|v| v.as_u64());
    let exp = claims.get("exp").and_then(|v| v.as_u64());
    ensure!(
        matches!((iat, exp), (Some(iat), Some(exp))
            if exp.checked_sub(iat).is_some_and(|ttl| ttl <= CONTROL_TOKEN_MAX_TTL_SECS)),
        "signing refused: a head token lives at most {CONTROL_TOKEN_MAX_TTL_SECS} s"
    );
    Ok(())
}
