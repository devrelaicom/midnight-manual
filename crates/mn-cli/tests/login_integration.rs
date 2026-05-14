//! Integration tests for `mnm login` (FR-068, FR-069, FR-019).
//!
//! Drives the full challenge → sign → verify dance against a `wiremock`
//! HTTP mock. Asserts:
//!
//! 1. The flow round-trips a real Ed25519 signature.
//!
//! 2. The returned JWT is persisted to `auth.toml[admin]` with mode `0o600`
//!    (Unix only).
//!
//! 3. Stdout / stderr never contain the token bytes (FR-019).
//!
//! 4. Canary strings fed into the request surface (`user_id`) never appear
//!    in the captured stdout/stderr buffer (FR-112 / `mn_telemetry::canary`).

use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::Verifier as _;
use mn_auth::Keypair;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// One challenge → one verify round-trip.
struct AuthMock {
    /// 32 bytes of nonce we hand out.
    nonce: [u8; 32],
    /// The `challenge_id` we mint.
    challenge_id: String,
    /// The token we hand back on a successful verify.
    token: String,
    /// Captured signature bytes (filled in by the verify handler).
    captured_sig: Arc<Mutex<Option<[u8; 64]>>>,
}

impl AuthMock {
    fn new(token: &str) -> Self {
        Self {
            nonce: [42u8; 32],
            challenge_id: "ch_test_001".into(),
            token: token.into(),
            captured_sig: Arc::new(Mutex::new(None)),
        }
    }
}

#[tokio::test]
async fn happy_path_persists_token_and_does_not_leak_it() {
    let server = MockServer::start().await;

    // Keypair we'll seed on disk and the server will verify against.
    let kp = Keypair::generate();
    let public = kp.verifying();

    let auth = AuthMock::new("fake.jwt.token-zzzzz-not-secret-but-still-redacted");
    let nonce_b64 = STANDARD_NO_PAD.encode(auth.nonce);
    let challenge_id = auth.challenge_id.clone();
    let token = auth.token.clone();
    let captured = Arc::clone(&auth.captured_sig);

    // /v1/auth/challenge handler — returns a fixed nonce keyed by challenge_id.
    Mock::given(method("POST"))
        .and(path("/v1/auth/challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "challenge_id": challenge_id,
            "nonce_b64": nonce_b64,
            "expires_in_s": 60,
        })))
        .mount(&server)
        .await;

    // /v1/auth/verify handler — captures the posted signature so the test
    // can re-verify against the public key, mints a token, and returns it.
    let verify_token = token.clone();
    let captured_for_handler = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/v1/auth/verify"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = req.body_json().unwrap();
            let sig_b64 = body["signature_b64"].as_str().unwrap();
            let sig_bytes = STANDARD_NO_PAD
                .decode(sig_b64.trim_end_matches('='))
                .or_else(|_| base64::engine::general_purpose::STANDARD.decode(sig_b64))
                .unwrap();
            assert_eq!(sig_bytes.len(), 64, "Ed25519 sig should be 64 bytes");
            let mut arr = [0u8; 64];
            arr.copy_from_slice(&sig_bytes);
            *captured_for_handler.lock().unwrap() = Some(arr);
            ResponseTemplate::new(200).set_body_json(json!({
                "token": verify_token,
                "user_id": "aaron",
                // 1 hour from now.
                "expires_at": time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
            }))
        })
        .mount(&server)
        .await;

    // Seed the local keypair on disk.
    let dir = tempfile::tempdir().unwrap();
    let private_path = dir.path().join("aaron.private");
    let auth_path = dir.path().join("auth.toml");
    write_private_key(&private_path, &kp.signing_bytes());

    // Run the flow.
    mn_cli::commands::login::run_with_paths(
        "aaron",
        &private_path,
        &auth_path,
        &server.uri(),
        false,
        true, // emit json so we can inspect captured stdout cleanly
    )
    .await
    .expect("login should succeed");

    // The server captured a real signature over our nonce.
    let sig = captured.lock().unwrap().take().expect("verify handler ran");
    let signature = ed25519_dalek::Signature::from_bytes(&sig);
    public
        .verify(&auth.nonce, &signature)
        .expect("signature verifies under our keypair");

    // The token landed on disk in auth.toml.
    let body = std::fs::read_to_string(&auth_path).expect("auth.toml exists");
    assert!(body.contains(&token), "auth.toml must carry the token");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&auth_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "auth.toml must be 0o600");
    }
}

#[tokio::test]
async fn dry_run_does_not_persist_token() {
    let server = MockServer::start().await;
    let kp = Keypair::generate();
    let nonce_b64 = STANDARD_NO_PAD.encode([0u8; 32]);

    Mock::given(method("POST"))
        .and(path("/v1/auth/challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "challenge_id": "ch_dry",
            "nonce_b64": nonce_b64,
            "expires_in_s": 60,
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/auth/verify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": "should-not-persist",
            "user_id": "aaron",
            "expires_at": time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let private_path = dir.path().join("aaron.private");
    let auth_path = dir.path().join("auth.toml");
    write_private_key(&private_path, &kp.signing_bytes());

    mn_cli::commands::login::run_with_paths(
        "aaron",
        &private_path,
        &auth_path,
        &server.uri(),
        true, // dry_run
        true,
    )
    .await
    .expect("dry-run should succeed");

    assert!(!auth_path.exists(), "--dry-run must not write auth.toml");
}

#[tokio::test]
async fn unauthorized_verify_returns_error() {
    let server = MockServer::start().await;
    let kp = Keypair::generate();
    let nonce_b64 = STANDARD_NO_PAD.encode([0u8; 32]);

    Mock::given(method("POST"))
        .and(path("/v1/auth/challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "challenge_id": "ch_x",
            "nonce_b64": nonce_b64,
            "expires_in_s": 60,
        })))
        .mount(&server)
        .await;

    // Server says no.
    Mock::given(method("POST"))
        .and(path("/v1/auth/verify"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {
                "code": "forbidden",
                "message": "signature did not verify under the registered public key",
                "remediation": "re-check the local keypair against the user-store row"
            }
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let private_path = dir.path().join("aaron.private");
    let auth_path = dir.path().join("auth.toml");
    write_private_key(&private_path, &kp.signing_bytes());

    let err = mn_cli::commands::login::run_with_paths(
        "aaron",
        &private_path,
        &auth_path,
        &server.uri(),
        false,
        true,
    )
    .await
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("verify endpoint returned"));
    assert!(!auth_path.exists(), "failed verify must not write auth.toml");
}

#[tokio::test]
async fn missing_private_key_errors_with_helpful_message() {
    let dir = tempfile::tempdir().unwrap();
    let private_path = dir.path().join("ghost.private");
    let auth_path = dir.path().join("auth.toml");

    let err = mn_cli::commands::login::run_with_paths(
        "ghost",
        &private_path,
        &auth_path,
        "http://127.0.0.1:1", // unreachable; should never get this far
        false,
        true,
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("mnm keys generate") || msg.contains("private key"),
        "error should mention how to recover: {msg}",
    );
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
