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
//! JSON-escaped form. Every outgoing structured log passes through
//! [`scrub::scrub_log`], which applies the same value-redaction to the serialized
//! log body/attributes. Every outgoing metric passes through
//! [`scrub::scrub_metric`], which first strips any attribute key outside
//! [`scrub::METRIC_ATTR_ALLOWLIST`] (metric attributes are free-form key/value
//! pairs, so this closed allow-list is the structural defense against a
//! free-form-string leak) and then applies the same value-redaction. Note this
//! covers secrets *configured at startup* (DSN, DB URL, API keys, signing
//! secrets, the on-disk auth tokens); it cannot redact a credential *minted at
//! runtime* (e.g. an OAuth token fetched mid-request) since that value is not
//! known when the scrubber is built. Dropping breadcrumbs above is the primary
//! defense for those. Redaction fails **closed**: if a secret is present but the
//! scrubbed event, log, or metric cannot be re-serialized, the scrubber drops
//! the payload rather than transmit the secret in cleartext.

use std::sync::Arc;

pub mod scrub;

pub use scrub::scrub_event;

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

/// Resolve the head-sampling rate for a root transaction.
///
/// Precedence: the pillar gate wins (traces disabled → never sample); then an
/// inherited distributed-trace decision is honored exactly (sampled → 1.0,
/// dropped → 0.0); otherwise fall back to the configured base rate. A `NaN`
/// base rate normalizes to `0.0` (fail-safe — never hand the SDK a NaN rate).
#[must_use]
fn resolve_trace_sample_rate(
    enable_traces: bool,
    base_rate: f32,
    parent_sampled: Option<bool>,
) -> f32 {
    if !enable_traces {
        return 0.0;
    }
    let base = if base_rate.is_nan() {
        0.0
    } else {
        base_rate.clamp(0.0, 1.0)
    };
    parent_sampled.map_or(base, |s| if s { 1.0 } else { 0.0 })
}

/// Knobs for [`init`].
// The four `bool`s are independent per-pillar/gate toggles (admin presence,
// logs, metrics, traces), not encodable as a single state machine or
// two-variant enum — `struct_excessive_bools` does not apply here.
#[allow(clippy::struct_excessive_bools)]
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
    /// Enable structured logs (info+ tracing events → Sentry logs). Gated.
    pub enable_logs: bool,
    /// Enable metrics (counter/gauge/distribution). Gated.
    pub enable_metrics: bool,
    /// Enable performance traces/spans. Gated.
    pub enable_traces: bool,
    /// Head sample rate for ROOT transactions with no incoming trace (staff
    /// requests continue an incoming sampled trace regardless). 0.0..=1.0.
    pub traces_sample_rate: f32,
    /// Surface tag applied to every event/transaction: `"cli"` | `"mcp"` |
    /// `"server"`.
    pub surface: &'a str,
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
    // `secrets` is captured by three closures below (`before_send`,
    // `before_send_log`, `before_send_metric`), initialized in source order.
    // Rust moves each closure's captures in order, so every closure but the
    // last must `.clone()`; only the final one may take the move (a `.clone()`
    // on the last would trip `clippy::redundant_clone` under `-D warnings`).
    // No clone is needed here: `opts.secrets` is not read anywhere else, so
    // this is a plain move out of `opts`.
    let secrets = opts.secrets;
    let admin_user_id = opts.admin_user_id;
    let enable_traces = opts.enable_traces;
    let traces_sample_rate = opts.traces_sample_rate;
    let surface = opts.surface.to_owned();

    let guard = sentry::init(sentry::ClientOptions {
        dsn: Some(parsed_dsn),
        release: Some(opts.release.to_owned().into()),
        environment: Some(environment.into()),
        send_default_pii: false,
        // Root-transaction head sampling. A request that continues an incoming
        // (sampled) `sentry-trace` inherits that decision exactly, so staff-originated
        // traces are kept whenever traces are enabled at all; only trace-less regular
        // traffic is rate-sampled.
        traces_sampler: Some(Arc::new(move |ctx| {
            resolve_trace_sample_rate(enable_traces, traces_sample_rate, ctx.sampled())
        })),
        enable_logs: opts.enable_logs,
        enable_metrics: opts.enable_metrics,
        before_send: Some(Arc::new({
            let secrets = secrets.clone();
            move |event| scrub_event(event, &secrets, admin_user_id.as_deref())
        })),
        before_send_log: Some(Arc::new({
            let secrets = secrets.clone();
            move |log| scrub::scrub_log(log, &secrets)
        })),
        before_send_metric: Some(Arc::new({
            // Final use of `secrets` — move, no clone needed.
            move |metric| scrub::scrub_metric(metric, &secrets)
        })),
        ..Default::default()
    });
    // Tag every event/transaction from this process with its surface.
    sentry::configure_scope(|scope| scope.set_tag("surface", &surface));
    Some(guard)
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
                enable_logs: false,
                enable_metrics: false,
                enable_traces: false,
                traces_sample_rate: 0.0,
                surface: "test",
            },
        );
        assert!(guard.is_none());
    }

    #[test]
    fn init_options_carry_pillar_toggles() {
        // Compile-level assertion that the new fields exist and are settable.
        let opts = InitOptions {
            admin_present: true,
            release: "0.0.0",
            default_environment: "test",
            admin_user_id: None,
            secrets: vec![],
            enable_logs: true,
            enable_metrics: true,
            enable_traces: true,
            traces_sample_rate: 0.25,
            surface: "cli",
        };
        assert!((opts.traces_sample_rate - 0.25).abs() < f32::EPSILON);
        assert_eq!(opts.surface, "cli");
    }

    // These branches return the literal constants `0.0`/`1.0` (not a computed
    // value), so bit-exact `assert_eq!` is the correct check; `clippy::float_cmp`
    // exists to catch imprecision from arithmetic, which does not apply here.
    #[test]
    #[allow(clippy::float_cmp)]
    fn trace_sample_rate_gate_off_is_zero() {
        assert_eq!(resolve_trace_sample_rate(false, 1.0, Some(true)), 0.0);
        assert_eq!(resolve_trace_sample_rate(false, 0.5, None), 0.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn trace_sample_rate_honors_parent_decision() {
        assert_eq!(resolve_trace_sample_rate(true, 0.1, Some(true)), 1.0);
        assert_eq!(resolve_trace_sample_rate(true, 0.1, Some(false)), 0.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn trace_sample_rate_falls_back_to_clamped_base() {
        assert!((resolve_trace_sample_rate(true, 0.25, None) - 0.25).abs() < f32::EPSILON);
        assert_eq!(resolve_trace_sample_rate(true, 5.0, None), 1.0);
        assert_eq!(resolve_trace_sample_rate(true, -1.0, None), 0.0);
        assert_eq!(resolve_trace_sample_rate(true, f32::NAN, None), 0.0);
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
                enable_logs: false,
                enable_metrics: false,
                enable_traces: false,
                traces_sample_rate: 0.0,
                surface: "test",
            },
        );
        assert!(guard.is_none());
    }
}
