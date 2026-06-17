//! Canary tests for `mnm login` (FR-019, FR-112).
//!
//! Drives the login flow end-to-end against a wiremock server, then asserts
//! that the user-facing output (the string `format_success` would have
//! printed) never contains:
//!
//! 1. The token bytes returned by the server.
//!
//! 2. Any [`mnm_telemetry::canary`] string fed in as a request input.
//!
//! `format_success` is the only place in `mnm login` that emits to stdout,
//! so checking its output for leaks is equivalent to grepping stdout.

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mnm_auth::Keypair;
use mnm_telemetry::canary::{find_first_match, CANARY_PREFIX};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn login_output_does_not_carry_token() {
    let server = MockServer::start().await;
    let kp = Keypair::generate();
    let nonce_b64 = STANDARD_NO_PAD.encode([1u8; 32]);

    // A *canary* token — if format_success ever echoes the token, the
    // canary suite's prefix is what we'll catch.
    let canary_token = format!("{CANARY_PREFIX}bearer_login_test_zzz");

    Mock::given(method("POST"))
        .and(path("/v1/auth/challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "challenge_id": "ch_canary",
            "nonce_b64": nonce_b64,
            "expires_in_s": 60,
        })))
        .mount(&server)
        .await;

    let token = canary_token.clone();
    Mock::given(method("POST"))
        .and(path("/v1/auth/verify"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "token": token,
                "user_id": "aaron",
                "expires_at": time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
            }))
        })
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let private_path = dir.path().join("aaron.private");
    let auth_path = dir.path().join("auth.toml");
    write_private_key(&private_path, &kp.signing_bytes());

    // Run the real flow against the mock.
    midnight_manual::commands::login::run_with_paths(
        "aaron",
        &private_path,
        &auth_path,
        &server.uri(),
        false,
        true,
    )
    .await
    .expect("login should succeed");

    // Token landed on disk (proof the flow actually executed).
    let body = std::fs::read_to_string(&auth_path).unwrap();
    assert!(body.contains(&canary_token));

    // Now exercise the same renderer with the same inputs — the user-facing
    // string MUST NOT carry the token.
    let rendered = midnight_manual::commands::login::format_success(
        "aaron",
        time::OffsetDateTime::from_unix_timestamp(
            time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        )
        .unwrap(),
        false,
        &auth_path,
        true,
    );
    assert!(
        find_first_match(&rendered).is_none(),
        "canary appeared in login output: {rendered:?}",
    );
    assert!(!rendered.contains(&canary_token), "token bytes leaked into render: {rendered}",);
}

#[tokio::test]
async fn login_human_renderer_prints_user_id_only() {
    // The human renderer legitimately prints `logged in as {user_id}` — we
    // want to assert that nothing *else* sneaks in. So we feed a canary
    // user_id and confirm the only occurrence is in the expected leading
    // position; the body never carries token / path / time-encoded bits
    // beside the canary.
    let canary_user_id = format!("{CANARY_PREFIX}user_login_canary_zzz");
    let rendered = midnight_manual::commands::login::format_success(
        &canary_user_id,
        time::OffsetDateTime::from_unix_timestamp(
            time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        )
        .unwrap(),
        false,
        std::path::Path::new("/tmp/auth.toml"),
        false, // human mode
    );
    assert!(rendered.starts_with(&format!("logged in as {canary_user_id};")));
    // The line is short enough that only the user_id appears — anything
    // else would be a structural leak. Strip the prefix and confirm no
    // additional canary fragment.
    let rest = rendered.trim_start_matches(&format!("logged in as {canary_user_id}"));
    assert!(!rest.contains(CANARY_PREFIX), "canary appeared more than once: {rendered:?}",);
}

#[cfg(unix)]
fn write_private_key(path: &std::path::Path, seed: &[u8; 32]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    f.write_all(seed).unwrap();
    f.flush().unwrap();
}

#[cfg(not(unix))]
fn write_private_key(path: &std::path::Path, seed: &[u8; 32]) {
    std::fs::write(path, seed).unwrap();
}
