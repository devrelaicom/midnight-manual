//! Opt-in Sentry error/crash reporting for the `midnight-manual` server and CLI.
//!
//! This crate is **separate** from the product analytics in `mnm-telemetry`:
//! `mnm-telemetry` is opt-*out* usage telemetry, while this is opt-*in* crash /
//! error reporting wired into `tracing` and the panic hook. Nothing here runs
//! at runtime unless the operator explicitly turns it on.
//!
//! # Gating
//!
//! Sentry initializes only when **all** applicable conditions hold; otherwise
//! there is zero init, no network, and no guard:
//!
//! - [`KEY_ENV`] (`MIDNIGHT_MANUAL_SENTRY_KEY`) is set and non-empty — the DSN.
//!   (server + client)
//! - [`ENABLE_ENV`] (`MIDNIGHT_MANUAL_SENTRY_ENABLE`) is exactly `"1"`.
//!   (server + client)
//! - **Client only:** the local `auth.toml` has an `[admin]` section present.
//!   The client gates on *presence* of the section, not token validity — the
//!   admin JWT has a 1h TTL, so gating on validity would make Sentry flap on
//!   and off hourly; section presence is the stable "maintainer machine"
//!   signal. The server passes `admin_present = true` (the admin gate is N/A
//!   server-side).
//!
//! The single source of truth for the boolean gate is [`sentry_enabled`].
//!
//! # Privacy
//!
//! Only `error`-level `tracing` events (and panics) are sent — [`tracing_layer`]
//! drops the default `info`/`warn` breadcrumbs so request-scoped context (logins,
//! org names, error strings) does not leave the machine.
//!
//! Every outgoing event passes through [`scrub_event`], which drops the hostname,
//! pins the Sentry user to a single admin id (or clears it), and value-redacts
//! every known secret from the serialized event body — both its verbatim and its
//! JSON-escaped form. Note this covers secrets *configured at startup* (DSN, DB
//! URL, API keys, signing secrets, the on-disk auth tokens); it cannot redact a
//! credential *minted at runtime* (e.g. an OAuth token fetched mid-request) since
//! that value is not known when the scrubber is built. Dropping breadcrumbs above
//! is the primary defense for those.

use std::sync::Arc;

/// Environment variable holding the Sentry DSN (the project key). Setting this
/// to a non-empty value is one of the gate conditions; the value itself is the
/// DSN passed to Sentry. (server + client)
pub const KEY_ENV: &str = "MIDNIGHT_MANUAL_SENTRY_KEY";

/// Environment variable that must equal the exact string `"1"` to enable
/// Sentry. Any other value (including unset, `"0"`, or `"true"`) disables it.
/// (server + client)
pub const ENABLE_ENV: &str = "MIDNIGHT_MANUAL_SENTRY_ENABLE";

/// Environment variable that overrides the Sentry `environment` tag. When unset
/// or empty, [`InitOptions::default_environment`] is used instead.
pub const ENVIRONMENT_ENV: &str = "MIDNIGHT_MANUAL_SENTRY_ENVIRONMENT";

/// The single source of truth for the activation gate.
///
/// Returns `true` iff the DSN key is present and non-empty, the enable flag is
/// exactly `"1"`, and the admin gate is satisfied (`admin_present`). The server
/// always passes `admin_present = true`; the client passes its real `auth.toml`
/// `[admin]`-section presence check.
#[must_use]
pub fn sentry_enabled(key: Option<&str>, enable: Option<&str>, admin_present: bool) -> bool {
    key.is_some_and(|k| !k.is_empty()) && enable == Some("1") && admin_present
}

/// True iff the env gates ([`KEY_ENV`] set & non-empty, [`ENABLE_ENV`] == `"1"`)
/// pass. Equivalent to `sentry_enabled(key, enable, true)` — i.e. it ignores the
/// admin gate.
///
/// The client uses this as a cheap pre-check so it can avoid reading `auth.toml`
/// when the environment variables already disqualify Sentry.
#[must_use]
pub fn env_gate_passes(env: &impl mnm_core::config::ConfigEnv) -> bool {
    let key = env.var(KEY_ENV);
    let enable = env.var(ENABLE_ENV);
    sentry_enabled(key.as_deref(), enable.as_deref(), true)
}

/// Knobs for [`init`].
pub struct InitOptions<'a> {
    /// Server passes `true` (admin gate N/A); client passes the real
    /// `auth.toml` `[admin]`-section presence check.
    pub admin_present: bool,
    /// Binary version, e.g. `env!("CARGO_PKG_VERSION")` — becomes the Sentry
    /// `release`.
    pub release: &'a str,
    /// Fallback environment tag when [`ENVIRONMENT_ENV`] is unset (e.g.
    /// `"production"` / `"development"`).
    pub default_environment: &'a str,
    /// Admin user id to set as the (only) Sentry user identifier; `None` on the
    /// server.
    pub admin_user_id: Option<String>,
    /// Secret values to redact from every outgoing event.
    pub secrets: Vec<String>,
}

/// Initialize Sentry iff the gate passes.
///
/// Returns the guard (hold it for the process lifetime so buffered events flush
/// on shutdown) or `None` when disabled (no init, no network). Reads
/// [`KEY_ENV`] / [`ENABLE_ENV`] / [`ENVIRONMENT_ENV`] via `env`.
///
/// When the DSN is set but fails to parse, this logs a `warn` and returns
/// `None` rather than panicking — a malformed DSN should not take down the
/// process.
#[must_use]
pub fn init(
    env: &impl mnm_core::config::ConfigEnv,
    opts: InitOptions<'_>,
) -> Option<sentry::ClientInitGuard> {
    let key = env.var(KEY_ENV);
    let enable = env.var(ENABLE_ENV);
    if !sentry_enabled(key.as_deref(), enable.as_deref(), opts.admin_present) {
        return None;
    }

    // The gate guarantees a non-empty key here.
    let dsn = key.expect("gate guarantees KEY_ENV present");
    let parsed_dsn = match dsn.parse::<sentry::types::Dsn>() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "MIDNIGHT_MANUAL_SENTRY_KEY is not a valid DSN; Sentry disabled");
            return None;
        }
    };

    let environment = env
        .var(ENVIRONMENT_ENV)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| opts.default_environment.to_owned());
    let secrets = opts.secrets;
    let admin_user_id = opts.admin_user_id;

    let guard = sentry::init(sentry::ClientOptions {
        dsn: Some(parsed_dsn),
        release: Some(opts.release.to_owned().into()),
        environment: Some(environment.into()),
        send_default_pii: false,
        before_send: Some(Arc::new(move |event| {
            scrub_event(event, &secrets, admin_user_id.as_deref())
        })),
        ..Default::default()
    });
    Some(guard)
}

/// `before_send` body: scrub PII and secrets from an outgoing event.
///
/// Drops the hostname, pins the user to the admin id (or clears it), and
/// value-redacts every known secret from the serialized event.
///
/// Fails **closed** on the secret-redaction path: if the event cannot be
/// serialized for inspection, or the redacted JSON cannot be re-parsed back into
/// an `Event`, this returns `None` (dropping the event) rather than fall back to
/// the pre-scrub original and risk shipping a secret in cleartext. Events with no
/// secrets configured to redact are always returned (still scrubbed of PII).
#[must_use]
pub fn scrub_event(
    mut event: sentry::protocol::Event<'static>,
    secrets: &[String],
    admin_user_id: Option<&str>,
) -> Option<sentry::protocol::Event<'static>> {
    // 1. Hostname is PII.
    event.server_name = None;

    // 2. Normalize the user: only the admin id (drops ip/email/username), or
    //    nothing when there is no admin id.
    event.user = admin_user_id.map(|id| sentry::protocol::User {
        id: Some(id.to_owned()),
        ..Default::default()
    });

    // 3. Value-redact secrets by serializing the whole event, replacing every
    //    known secret in the JSON, and deserializing back. This is field-agnostic
    //    (it catches a secret wherever it landed: `extra`, `exception`,
    //    `contexts`, `request`, breadcrumbs, ...) and trivially testable. For
    //    each secret we redact both its verbatim bytes and its JSON-escaped form,
    //    because a secret containing `"` or `\` (e.g. a DB-URL password)
    //    serializes escaped and would otherwise dodge a raw match. It catches
    //    only secrets that appear verbatim once serialized — a value
    //    transformed/encoded before landing in the event is not covered.
    //
    //    The `>= 8` filter avoids over-redacting on trivially short strings; all
    //    configured secrets (API keys, JWTs, DB URLs, DSNs) are long. Secrets
    //    shorter than 8 chars are intentionally NOT redacted — do not add a short
    //    secret to the list expecting protection.
    let relevant: Vec<&String> = secrets.iter().filter(|s| s.len() >= 8).collect();
    if !relevant.is_empty() {
        // Serialize the whole event so a secret is caught wherever it landed. If
        // serialization fails we cannot prove the event is secret-free, so fail
        // CLOSED: drop the event rather than risk shipping an unredacted secret.
        let mut json = match serde_json::to_string(&event) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "sentry scrub: event serialization failed; dropping event to avoid leaking secrets"
                );
                return None;
            }
        };

        let mut changed = false;
        for s in relevant {
            for needle in secret_needles(s) {
                if json.contains(needle.as_str()) {
                    json = json.replace(needle.as_str(), "[REDACTED]");
                    changed = true;
                }
            }
        }

        if changed {
            // Re-parse the redacted JSON. If it does not round-trip back into an
            // `Event`, the pre-scrub `event` still holds the secret verbatim, so
            // fail CLOSED here too: drop the event instead of falling back to the
            // unredacted original. Rich `error`-level events (exception, contexts,
            // debug-images) are the ones most likely to trip this path.
            match serde_json::from_str::<sentry::protocol::Event<'static>>(&json) {
                Ok(scrubbed) => event = scrubbed,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "sentry scrub: redacted event failed to re-parse; dropping event to avoid leaking secrets"
                    );
                    return None;
                }
            }
        }
    }

    Some(event)
}

/// The strings to search for when redacting `secret` from a serialized event:
/// the verbatim value plus its JSON-escaped form (without the surrounding quotes
/// serde adds), so a secret containing `"` or `\` is caught in the escaped event
/// body too.
fn secret_needles(secret: &str) -> Vec<String> {
    let mut needles = vec![secret.to_owned()];
    if let Ok(json_lit) = serde_json::to_string(secret) {
        // `to_string` of a `&str` is always `"..."` (len >= 2); strip the quotes.
        if json_lit.len() >= 2 {
            let inner = &json_lit[1..json_lit.len() - 1];
            if inner != secret {
                needles.push(inner.to_owned());
            }
        }
    }
    needles
}

/// The `sentry-tracing` layer to attach to a `tracing_subscriber` registry so
/// `error`-level events and panics are captured. Inert if Sentry isn't
/// initialized.
///
/// Only `ERROR` events are captured (as Sentry events). The default layer would
/// also ship `info`/`warn` logs as breadcrumbs; we deliberately drop those to
/// keep request-scoped context off the wire (see the crate `# Privacy` docs).
/// Panics are captured independently by the Sentry panic integration, so this
/// filter does not affect crash capture.
#[must_use]
pub fn tracing_layer<S>() -> sentry_tracing::SentryLayer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    sentry_tracing::layer().event_filter(|md| match *md.level() {
        tracing::Level::ERROR => sentry_tracing::EventFilter::Event,
        _ => sentry_tracing::EventFilter::Ignore,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn set(mut self, k: &str, v: &str) -> Self {
            self.0.insert(k.into(), v.into());
            self
        }
    }

    impl mnm_core::config::ConfigEnv for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn gate_false_when_key_unset() {
        assert!(!sentry_enabled(None, Some("1"), true));
    }

    #[test]
    fn gate_false_when_key_empty() {
        assert!(!sentry_enabled(Some(""), Some("1"), true));
    }

    #[test]
    fn gate_false_when_enable_not_exactly_one() {
        assert!(!sentry_enabled(Some("dsn"), None, true));
        assert!(!sentry_enabled(Some("dsn"), Some("0"), true));
        assert!(!sentry_enabled(Some("dsn"), Some("true"), true));
    }

    #[test]
    fn gate_false_when_admin_absent() {
        assert!(!sentry_enabled(Some("dsn"), Some("1"), false));
    }

    #[test]
    fn gate_true_when_all_present() {
        assert!(sentry_enabled(Some("dsn"), Some("1"), true));
    }

    #[test]
    fn env_gate_ignores_admin() {
        // Key + enable set -> passes regardless of admin (it always assumes true).
        let env = FakeEnv::default()
            .set(KEY_ENV, "https://pub@example.ingest.sentry.io/123")
            .set(ENABLE_ENV, "1");
        assert!(env_gate_passes(&env));
    }

    #[test]
    fn env_gate_false_without_enable() {
        let env = FakeEnv::default().set(KEY_ENV, "dsn");
        assert!(!env_gate_passes(&env));
    }

    #[test]
    fn env_gate_false_without_key() {
        let env = FakeEnv::default().set(ENABLE_ENV, "1");
        assert!(!env_gate_passes(&env));
    }

    #[test]
    fn scrub_redacts_secret_in_extra_and_message() {
        let secret = "voyage-secret-abcdefgh";
        let mut extra = sentry::protocol::Map::new();
        extra.insert("api_key".into(), serde_json::Value::String(secret.to_owned()));
        let event = sentry::protocol::Event {
            message: Some(format!("boom while using {secret}")),
            extra,
            ..Default::default()
        };

        let scrubbed = scrub_event(event, &[secret.to_owned()], None).expect("always Some");
        let json = serde_json::to_string(&scrubbed).expect("serialize");

        assert!(!json.contains(secret), "secret must be absent after scrub: {json}");
        assert!(json.contains("[REDACTED]"), "redaction marker must be present: {json}");
    }

    #[test]
    fn scrub_redacts_secret_with_json_special_chars() {
        // A DB-URL-style password containing `"` and `\` serializes JSON-escaped,
        // so the scrub must match the escaped representation, not just the raw
        // bytes. (Raw string: the `\` and `r` are two literal chars, not a CR.)
        let secret = r#"pa"ss\rd-abcdefgh"#;
        let mut extra = sentry::protocol::Map::new();
        extra.insert(
            "db".into(),
            serde_json::Value::String(format!("postgres://u:{secret}@host/db")),
        );
        let event = sentry::protocol::Event { extra, ..Default::default() };

        let scrubbed = scrub_event(event, &[secret.to_owned()], None).expect("always Some");
        let json = serde_json::to_string(&scrubbed).expect("serialize");

        // The escaped form that actually appears in the event JSON must be gone.
        assert!(!json.contains(r#"pa\"ss\\rd-abcdefgh"#), "JSON-escaped secret leaked: {json}");
        assert!(json.contains("[REDACTED]"), "redaction marker must be present: {json}");
    }

    #[test]
    fn scrub_clears_server_name() {
        let event = sentry::protocol::Event {
            server_name: Some("my-private-hostname".into()),
            ..Default::default()
        };

        let scrubbed = scrub_event(event, &[], None).expect("always Some");
        assert!(scrubbed.server_name.is_none(), "hostname must be cleared");
    }

    #[test]
    fn scrub_sets_user_to_admin_id() {
        // A pre-existing user with PII should be replaced wholesale.
        let event = sentry::protocol::Event {
            user: Some(sentry::protocol::User {
                email: Some("leak@example.com".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let scrubbed = scrub_event(event, &[], Some("aaron")).expect("always Some");
        let user = scrubbed.user.expect("user set to admin id");
        assert_eq!(user.id.as_deref(), Some("aaron"));
        assert!(user.email.is_none(), "pre-existing email must be dropped");
    }

    #[test]
    fn scrub_clears_user_when_no_admin_id() {
        let event = sentry::protocol::Event {
            user: Some(sentry::protocol::User {
                email: Some("leak@example.com".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let scrubbed = scrub_event(event, &[], None).expect("always Some");
        assert!(scrubbed.user.is_none(), "user must be cleared when no admin id");
    }

    #[test]
    fn scrub_ignores_short_secrets() {
        // A short string (< 8 chars) is not redacted, to avoid over-matching.
        let event = sentry::protocol::Event {
            message: Some("the value is abc".to_owned()),
            ..Default::default()
        };

        let scrubbed = scrub_event(event, &["abc".to_owned()], None).expect("short secret kept");
        let json = serde_json::to_string(&scrubbed).expect("serialize");
        assert!(json.contains("abc"), "short secret must not be redacted");
        assert!(!json.contains("[REDACTED]"));
    }

    #[test]
    fn scrub_redacts_secret_in_rich_event() {
        // Real `error`-level events carry an `exception` and rich `contexts`
        // (the `contexts`/`debug-images` features are enabled in Cargo.toml), not
        // just a bare message. Redaction must reach the secret wherever it landed
        // AND the redacted event must still round-trip back into an `Event`
        // (i.e. return `Some`, not fail closed) on this realistic payload.
        let secret = "postgres-pw-abcdefghijkl";

        let mut ctx = sentry::protocol::Map::new();
        ctx.insert(
            "database_url".into(),
            serde_json::Value::String(format!("postgres://user:{secret}@db.internal/app")),
        );
        let mut contexts = sentry::protocol::Map::new();
        contexts.insert("runtime".into(), sentry::protocol::Context::Other(ctx));

        let exception = sentry::protocol::Exception {
            ty: "ConfigError".to_owned(),
            value: Some(format!("could not connect using secret {secret}")),
            ..Default::default()
        };

        let event = sentry::protocol::Event {
            message: Some(format!("boom while starting up: {secret}")),
            exception: vec![exception].into(),
            contexts,
            ..Default::default()
        };

        let scrubbed =
            scrub_event(event, &[secret.to_owned()], None).expect("rich event round-trips");
        let json = serde_json::to_string(&scrubbed).expect("serialize");

        assert!(!json.contains(secret), "secret must be gone from every field: {json}");
        assert!(json.contains("[REDACTED]"), "redaction marker must be present: {json}");
    }

    #[test]
    fn scrub_fails_closed_when_redacted_event_cannot_reparse() {
        // The security-critical guard (issue #166): if redaction produces JSON
        // that no longer round-trips into an `Event`, the event MUST be dropped
        // (`None`) — it must never be returned with the pre-scrub secret intact.
        //
        // We trigger a reparse failure deterministically. `event_id` is a
        // strictly-typed UUID that serializes to 32 hex chars; registering that
        // exact hex as a "secret" makes redaction rewrite `"event_id":"<hex>"`
        // into `"event_id":"[REDACTED]"`, which no longer deserializes as a UUID.
        // This stands in for any rich-event field whose redaction breaks the
        // schema round-trip.
        let event = sentry::protocol::Event::default();

        // The exact serialized id, byte-for-byte what `scrub_event` produces
        // internally, so the needle matches.
        let json = serde_json::to_string(&event).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let event_id_hex = value["event_id"]
            .as_str()
            .expect("event_id serializes as a string")
            .to_owned();
        assert!(event_id_hex.len() >= 8, "event_id hex must clear the length filter");

        let result = scrub_event(event, &[event_id_hex], None);
        assert!(
            result.is_none(),
            "a redacted event that cannot re-parse must be dropped (fail closed), \
             not returned unscrubbed"
        );
    }

    #[test]
    fn init_returns_none_when_disabled() {
        // No env at all -> no init, no guard.
        let env = FakeEnv::default();
        let guard = init(
            &env,
            InitOptions {
                admin_present: true,
                release: "0.0.0",
                default_environment: "test",
                admin_user_id: None,
                secrets: vec![],
            },
        );
        assert!(guard.is_none());
    }

    #[test]
    fn init_returns_none_on_invalid_dsn() {
        // Gate passes (key present + enable=1 + admin), but the DSN is garbage:
        // init must degrade to None rather than panic.
        let env = FakeEnv::default()
            .set(KEY_ENV, "not-a-valid-dsn")
            .set(ENABLE_ENV, "1");
        let guard = init(
            &env,
            InitOptions {
                admin_present: true,
                release: "0.0.0",
                default_environment: "test",
                admin_user_id: None,
                secrets: vec![],
            },
        );
        assert!(guard.is_none());
    }
}
