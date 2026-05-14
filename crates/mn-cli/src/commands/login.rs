//! `mnm login --user-id <id>` — admin auth handshake (FR-068, FR-069).
//!
//! Flow:
//!
//! 1. Load the local Ed25519 signing key from
//!    `<config_home>/keys/<user_id>.private` (chmod-checked).
//!
//! 2. `POST /v1/auth/challenge {user_id}` → `{challenge_id, nonce_b64}`.
//!
//! 3. Decode the nonce, sign with the local key, base64-encode the signature.
//!
//! 4. `POST /v1/auth/verify {challenge_id, signature_b64}` →
//!    `{token, user_id, expires_at}`.
//!
//! 5. Persist `{token, expires_at}` to `<config_home>/auth.toml` under
//!    `[admin]` via [`mn_core::auth_file::AuthFile::write_admin_token`], which
//!    handles the `0o600` discipline.
//!
//! FR-019 — the token is never logged. On `--json` we emit a single NDJSON
//! line that carries `user_id` + `expires_at` but NOT the token; on the human
//! path we print an expiry hint but not the bytes.

use anyhow::{anyhow, Context as _, Result};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use clap::Args as ClapArgs;
use mn_auth::Keypair;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::commands::keys::load_private_key;

/// Args for `mnm login`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// User id to authenticate as. Must match a row in the server's user
    /// store and the basename of a local `<user_id>.private` keypair file.
    #[arg(long)]
    pub user_id: String,

    /// Run the full handshake without persisting the resulting token.
    #[arg(long)]
    pub dry_run: bool,
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error when the local keypair cannot be loaded, when the
/// challenge / verify HTTP round-trip fails, or when the token cannot be
/// persisted to `auth.toml`.
pub async fn run(args: Args, server_flag: Option<&str>, json: bool) -> Result<()> {
    if args.user_id.trim().is_empty() {
        return Err(anyhow!("--user-id must be non-empty"));
    }

    let env = mn_core::config::StdEnv;
    let private_path = mn_core::paths::private_key_path(&env, &args.user_id).ok_or_else(|| {
        anyhow!(
            "could not resolve keys dir (set XDG_CONFIG_HOME or HOME so we know where to find `<user_id>.private`)"
        )
    })?;
    let auth_path = mn_core::paths::auth_file_path(&env)
        .ok_or_else(|| anyhow!("could not resolve auth.toml path (set XDG_CONFIG_HOME or HOME)"))?;

    let server_url = crate::shared::resolve_server_url(server_flag);

    run_with_paths(&args.user_id, &private_path, &auth_path, &server_url, args.dry_run, json).await
}

/// Path-explicit driver, exposed for integration testing without env-var
/// gymnastics. The pub-API runs the same flow via [`run`].
///
/// # Errors
///
/// Same as [`run`].
pub async fn run_with_paths(
    user_id: &str,
    private_path: &std::path::Path,
    auth_path: &std::path::Path,
    server_url: &str,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let seed = load_private_key(private_path).with_context(|| {
        format!("load private key for user `{user_id}` (run `mnm keys generate --user-id {user_id}` first?)")
    })?;
    let keypair = Keypair::from_signing_bytes(seed);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;

    let challenge = post_challenge(&client, server_url, user_id).await?;
    let nonce = decode_nonce(&challenge.nonce_b64)?;
    let signature = keypair.sign(&nonce);
    let signature_b64 = STANDARD_NO_PAD.encode(signature);
    let verified =
        post_verify(&client, server_url, &challenge.challenge_id, &signature_b64).await?;

    let expires_at = OffsetDateTime::from_unix_timestamp(verified.expires_at)
        .context("server returned out-of-range expires_at")?;

    if !dry_run {
        mn_core::auth_file::AuthFile::write_admin_token(
            auth_path,
            &verified.user_id,
            &verified.token,
            expires_at,
        )
        .with_context(|| format!("persist admin token to {}", auth_path.display()))?;
    }

    let rendered = format_success(user_id, expires_at, dry_run, auth_path, json);
    println!("{rendered}");
    Ok(())
}

#[derive(Debug, Serialize)]
struct ChallengeRequest<'a> {
    user_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChallengeResponse {
    challenge_id: String,
    nonce_b64: String,
    #[allow(dead_code)]
    #[serde(default)]
    expires_in_s: i64,
}

#[derive(Debug, Serialize)]
struct VerifyRequest<'a> {
    challenge_id: &'a str,
    signature_b64: &'a str,
}

#[derive(Debug, Deserialize)]
struct VerifyResponse {
    token: String,
    user_id: String,
    expires_at: i64,
}

async fn post_challenge(
    client: &reqwest::Client,
    server_url: &str,
    user_id: &str,
) -> Result<ChallengeResponse> {
    let resp = client
        .post(format!("{server_url}/v1/auth/challenge"))
        .json(&ChallengeRequest { user_id })
        .send()
        .await
        .context("send /v1/auth/challenge")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("challenge endpoint returned {status}: {}", redact_token_like(&body),));
    }
    resp.json::<ChallengeResponse>()
        .await
        .context("parse challenge response")
}

async fn post_verify(
    client: &reqwest::Client,
    server_url: &str,
    challenge_id: &str,
    signature_b64: &str,
) -> Result<VerifyResponse> {
    let resp = client
        .post(format!("{server_url}/v1/auth/verify"))
        .json(&VerifyRequest { challenge_id, signature_b64 })
        .send()
        .await
        .context("send /v1/auth/verify")?;
    let status = resp.status();
    if !status.is_success() {
        // Don't echo the body verbatim — verify failures over a stale token
        // could in theory carry sensitive material; we redact anything that
        // looks like a bearer fragment.
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("verify endpoint returned {status}: {}", redact_token_like(&body),));
    }
    resp.json::<VerifyResponse>()
        .await
        .context("parse verify response")
}

fn decode_nonce(nonce_b64: &str) -> Result<Vec<u8>> {
    STANDARD_NO_PAD
        .decode(nonce_b64.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(nonce_b64))
        .map_err(|e| anyhow!("server returned invalid nonce_b64: {e}"))
}

/// Trim long base64-y fragments out of an error message so we don't echo a
/// token we accidentally got back in an error envelope.
fn redact_token_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if word.len() > 40
            && word
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '=')
        {
            out.push_str("[redacted]");
        } else {
            out.push_str(word);
        }
        out.push(' ');
    }
    out.trim_end().to_owned()
}

#[derive(Debug, Serialize)]
struct LoginOutput<'a> {
    action: &'a str,
    user_id: &'a str,
    expires_at: String,
    expires_in_s: i64,
    auth_file: String,
    dry_run: bool,
}

/// Build the user-facing line(s) we'd print on a successful login. Pure —
/// no I/O — so tests can grep the result for canary substrings.
///
/// In `json` mode produces a one-line NDJSON record carrying `user_id`,
/// `expires_at`, `expires_in_s`, `auth_file`, and `dry_run` — but NEVER the
/// token bytes (FR-019).
///
/// In human mode produces a single user-friendly status line.
#[must_use]
pub fn format_success(
    user_id: &str,
    expires_at: OffsetDateTime,
    dry_run: bool,
    auth_path: &std::path::Path,
    json: bool,
) -> String {
    let now = OffsetDateTime::now_utc();
    let expires_in_s = (expires_at - now).whole_seconds();
    let expires_iso = expires_at
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    if json {
        let body = LoginOutput {
            action: "login",
            user_id,
            expires_at: expires_iso,
            expires_in_s,
            auth_file: auth_path.display().to_string(),
            dry_run,
        };
        serde_json::to_string(&body).unwrap_or_default()
    } else if dry_run {
        format!(
            "logged in as {user_id} (DRY RUN — token not persisted; would expire in {} min)",
            expires_in_s.max(0) / 60,
        )
    } else {
        format!(
            "logged in as {user_id}; admin token expires in {} min",
            expires_in_s.max(0) / 60,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_long_alnum_blobs() {
        let body = "verify failed token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let redacted = redact_token_like(body);
        assert!(!redacted.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn keeps_short_words_intact() {
        let body = "verify failed bad signature";
        let redacted = redact_token_like(body);
        assert_eq!(redacted, "verify failed bad signature");
    }

    #[test]
    fn decode_nonce_handles_unpadded_and_padded() {
        // 32 zero bytes
        let unpadded = STANDARD_NO_PAD.encode([0u8; 32]);
        let decoded = decode_nonce(&unpadded).unwrap();
        assert_eq!(decoded.len(), 32);

        let padded = base64::engine::general_purpose::STANDARD.encode([1u8; 32]);
        let decoded = decode_nonce(&padded).unwrap();
        assert_eq!(decoded.len(), 32);
        assert!(decoded.iter().all(|&b| b == 1));
    }

    #[test]
    fn decode_nonce_rejects_garbage() {
        let err = decode_nonce("!!!not-base64!!!").unwrap_err();
        assert!(err.to_string().contains("invalid"));
    }
}
