//! Integration tests for `mnm auth {status,logout,github}` (FR-115).
//!
//! `status` and `logout` are exercised against the path-explicit helpers
//! `status_with_path` / `logout_with_path` so we don't have to mutate the
//! process-global `XDG_CONFIG_HOME` (which is forbidden under the
//! workspace's `unsafe_code = "forbid"` lint).
//!
//! The CLI's local OAuth-callback listener is exercised end-to-end against
//! a real `reqwest::Client` hitting it on a free local port — the same
//! shape the real server's callback would emit. The full server-side
//! exchange is covered separately in `crates/midnight-manual-server/tests/github_oauth.rs`.

use std::path::PathBuf;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use time::OffsetDateTime;

fn seeded_auth_toml(dir: &std::path::Path, body: &str) -> PathBuf {
    let path = dir.join("auth.toml");
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

#[test]
fn status_renders_both_present_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let exp = OffsetDateTime::now_utc() + time::Duration::hours(1);
    let exp_rfc = exp
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let body = format!(
        r#"
schema_version = 1

[admin]
user_id = "aaron"
token = "jwt-admin"
expires_at = "{exp_rfc}"

[read_uplift]
github_login = "aaronbassett"
token = "ru-bearer"
expires_at = "{exp_rfc}"
"#,
    );
    let path = seeded_auth_toml(dir.path(), &body);

    // json=true so we exercise the structured renderer too.
    midnight_manual::commands::auth::status_with_path(&path, true).unwrap();
}

#[test]
fn status_handles_absent_auth_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.toml");
    midnight_manual::commands::auth::status_with_path(&path, false).unwrap();
}

#[test]
fn logout_clears_read_uplift_only() {
    let dir = tempfile::tempdir().unwrap();
    let exp = OffsetDateTime::now_utc() + time::Duration::hours(1);
    let exp_rfc = exp
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let body = format!(
        r#"
schema_version = 1

[admin]
user_id = "aaron"
token = "jwt-admin"
expires_at = "{exp_rfc}"

[read_uplift]
github_login = "aaronbassett"
token = "ru-bearer"
expires_at = "{exp_rfc}"
"#,
    );
    let path = seeded_auth_toml(dir.path(), &body);

    midnight_manual::commands::auth::logout_with_path(&path, true).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("jwt-admin"), "admin section preserved");
    assert!(!after.contains("ru-bearer"), "read_uplift cleared");
}

#[test]
fn logout_when_no_token_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_auth_toml(dir.path(), "schema_version = 1\n");
    midnight_manual::commands::auth::logout_with_path(&path, true).unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after.trim(), "schema_version = 1");
}

#[tokio::test]
async fn local_listener_captures_oauth_callback() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let token = "jwt.example.123";
    let github_login = "aaron";
    let exp = OffsetDateTime::now_utc().unix_timestamp() + 86_400;
    let url = format!(
        "http://127.0.0.1:{port}/oauth?token={token}&github_login={github_login}&expires_at={exp}"
    );

    let listener_task = tokio::spawn(async move {
        midnight_manual::commands::auth::run_with_paths(&listener, Duration::from_secs(10))
            .await
            .unwrap()
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let params = listener_task.await.unwrap();
    assert_eq!(params.get("token").map(String::as_str), Some(token));
    assert_eq!(params.get("github_login").map(String::as_str), Some(github_login));
    assert_eq!(params.get("expires_at").map(String::as_str), Some(exp.to_string().as_str()),);
}

#[tokio::test]
async fn local_listener_ignores_non_oauth_paths() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let listener_task = tokio::spawn(async move {
        midnight_manual::commands::auth::run_with_paths(&listener, Duration::from_secs(5))
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Disable HTTP keep-alive / connection pooling. The server writes
    // `Connection: close` and `socket.shutdown().await` after each
    // response, so a pooled second request can race with the FIN and
    // intermittently fail. Forcing a fresh TCP connection per call
    // makes this test deterministic.
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    // First a 404 path — the listener should respond 404 and keep listening.
    let r = client
        .get(format!("http://127.0.0.1:{port}/favicon.ico"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::NOT_FOUND);

    // Then the real /oauth request that the listener should latch on.
    let token = STANDARD_NO_PAD.encode([1u8; 32]);
    let r = client
        .get(format!(
            "http://127.0.0.1:{port}/oauth?token={token}&github_login=x&expires_at=1700000000"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::OK);

    let params = listener_task.await.unwrap();
    assert_eq!(params.get("token"), Some(&token));
}

#[test]
fn persist_read_uplift_writes_0600_on_unix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.toml");
    let exp = OffsetDateTime::now_utc() + time::Duration::days(30);
    midnight_manual::commands::auth::persist_read_uplift(&path, "aaron", "ru-bearer-xyz", exp)
        .unwrap();

    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("ru-bearer-xyz"));
    assert!(body.contains("aaron"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
