//! Value-redaction scrubbers applied to every outgoing Sentry payload via the
//! `before_send*` hooks wired up in [`crate::init`].
//!
//! Each scrubber follows the same fail-**closed** strategy: serialize the
//! whole payload to JSON, replace every known-secret needle (verbatim and
//! JSON-escaped), and reparse. If the reparse fails, the payload is DROPPED
//! rather than risk shipping a secret in cleartext. See the crate-level
//! `# Privacy` docs for the full rationale.

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

/// `before_send_log` body: value-redact known secrets from an outgoing log.
///
/// Same field-agnostic, fail-closed strategy as [`scrub_event`]: serialize the
/// whole log, replace every known-secret needle, reparse. If reparse fails, the
/// log is DROPPED (`None`) rather than risk shipping a secret. Logs with no
/// configured secrets are returned unchanged.
#[must_use]
pub fn scrub_log(log: sentry::protocol::Log, secrets: &[String]) -> Option<sentry::protocol::Log> {
    let relevant: Vec<&String> = secrets.iter().filter(|s| s.len() >= 8).collect();
    if relevant.is_empty() {
        return Some(log);
    }
    let mut json = match serde_json::to_string(&log) {
        Ok(j) => j,
        Err(error) => {
            tracing::warn!(%error, "sentry scrub: log serialize failed; dropping to avoid leak");
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
    if !changed {
        return Some(log);
    }
    match serde_json::from_str::<sentry::protocol::Log>(&json) {
        Ok(scrubbed) => Some(scrubbed),
        Err(error) => {
            tracing::warn!(%error, "sentry scrub: redacted log failed to reparse; dropping");
            None
        }
    }
}

/// Attribute keys a metric is permitted to carry. Anything else is a potential
/// free-form-string leak and is stripped. Keep this list closed and reviewed.
pub const METRIC_ATTR_ALLOWLIST: &[&str] = &[
    "topic",
    "outcome",
    "surface",
    "mode",
    "reranker",
    "code_mode",
];

/// `before_send_metric` body: enforce the attribute-key allow-list, then apply
/// the same fail-closed secret redaction as [`scrub_event`]/[`scrub_log`].
///
/// The allow-list is a structural defense: metric attributes are free-form
/// key/value pairs, so anything not in [`METRIC_ATTR_ALLOWLIST`] is dropped
/// *before* redaction runs, closing off arbitrary-string leaks (e.g. a
/// `raw_query` attribute) that value-redaction alone cannot catch. Same
/// fail-closed strategy as [`scrub_log`]: serialize, replace every
/// known-secret needle, reparse. If reparse fails, the metric is DROPPED
/// (`None`) rather than risk shipping a secret. Metrics with no configured
/// secrets are returned unchanged (still allow-list-filtered).
#[must_use]
pub fn scrub_metric(
    mut metric: sentry::protocol::Metric,
    secrets: &[String],
) -> Option<sentry::protocol::Metric> {
    metric
        .attributes
        .retain(|k, _| METRIC_ATTR_ALLOWLIST.contains(&k.as_ref()));

    let relevant: Vec<&String> = secrets.iter().filter(|s| s.len() >= 8).collect();
    if relevant.is_empty() {
        return Some(metric);
    }
    let mut json = match serde_json::to_string(&metric) {
        Ok(j) => j,
        Err(error) => {
            tracing::warn!(%error, "sentry scrub: metric serialize failed; dropping to avoid leak");
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
    if !changed {
        return Some(metric);
    }
    match serde_json::from_str::<sentry::protocol::Metric>(&json) {
        Ok(scrubbed) => Some(scrubbed),
        Err(error) => {
            tracing::warn!(%error, "sentry scrub: redacted metric failed to reparse; dropping");
            None
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn scrub_log_redacts_secret_in_body_and_attributes() {
        let secret = "voyage-secret-abcdefgh";
        // `sentry::protocol::Log` (unlike `Event`) does not derive `Default`, so
        // every field must be set explicitly — mirroring how `sentry-tracing`
        // itself builds a `Log` in its `converters.rs`.
        let mut log = sentry::protocol::Log {
            level: sentry::protocol::LogLevel::Info,
            body: format!("calling api with {secret}"),
            trace_id: None,
            timestamp: std::time::SystemTime::now(),
            severity_number: None,
            attributes: sentry::protocol::Map::new(),
        };
        log.attributes
            .insert("api_key".into(), sentry::protocol::LogAttribute::from(secret.to_owned()));

        let scrubbed = scrub_log(log, &[secret.to_owned()]).expect("no reparse failure here");
        let json = serde_json::to_string(&scrubbed).expect("serialize");
        assert!(!json.contains(secret), "secret leaked: {json}");
        assert!(json.contains("[REDACTED]"));
    }

    #[test]
    fn scrub_log_fails_closed_when_redacted_log_cannot_reparse() {
        // The `scrub_log` counterpart to
        // `scrub_fails_closed_when_redacted_event_cannot_reparse` (issue #166):
        // if redaction produces JSON that no longer round-trips into a `Log`,
        // the log MUST be dropped (`None`) — it must never be returned with the
        // pre-scrub secret still resolvable.
        //
        // We trigger a reparse failure deterministically via `trace_id`, a
        // strictly-typed 16-byte id (`#[serde(try_from = "String", into =
        // "String")]`) that serializes to exactly 32 lowercase hex chars and
        // deserializes via `hex::decode_to_slice` into a fixed 16-byte buffer.
        // Registering that exact hex string as a "secret" makes redaction
        // rewrite `"trace_id":"<hex>"` into `"trace_id":"[REDACTED]"` — and
        // `"[REDACTED]"` is neither valid hex nor the right length, so
        // `TraceId`'s `FromStr`/`TryFrom<String>` impl errors and the whole
        // `Log` fails to reparse.
        let trace_id = sentry::protocol::TraceId::default();
        // `sentry::protocol::Log` does not derive `Default` (see the test
        // above), so every field is set explicitly.
        let log = sentry::protocol::Log {
            level: sentry::protocol::LogLevel::Info,
            body: "unrelated body, the secret only lives in trace_id".to_owned(),
            trace_id: Some(trace_id),
            timestamp: std::time::SystemTime::now(),
            severity_number: None,
            attributes: sentry::protocol::Map::new(),
        };

        // The exact serialized id, byte-for-byte what `scrub_log` produces
        // internally, so the needle matches.
        let trace_id_hex = trace_id.to_string();
        assert_eq!(trace_id_hex.len(), 32, "TraceId must serialize to 32 hex chars");
        assert!(trace_id_hex.len() >= 8, "trace_id hex must clear the length filter");

        let result = scrub_log(log, &[trace_id_hex]);
        assert!(
            result.is_none(),
            "a redacted log that cannot re-parse must be dropped (fail closed), \
             not returned unscrubbed"
        );
    }

    #[test]
    fn scrub_metric_drops_unlisted_string_attributes_and_redacts_secrets() {
        let secret = "db-secret-abcdefgh";
        // `sentry::protocol::Metric` (like `Log`) does not derive `Default` and
        // there is no `sentry::metrics::Metric` builder type — the value that
        // actually flows through `before_send_metric` is the wire/protocol
        // `Metric` (see `ClientOptions::before_send_metric: Option<BeforeCallback<Metric>>`
        // in sentry-core, importing `crate::protocol::Metric`). Every field is
        // set explicitly, mirroring the `scrub_log` tests above.
        let mut metric = sentry::protocol::Metric {
            r#type: sentry::protocol::MetricType::Counter,
            name: "search.requests".into(),
            value: 1.0,
            timestamp: std::time::SystemTime::now(),
            trace_id: sentry::protocol::TraceId::default(),
            span_id: None,
            unit: None,
            attributes: sentry::protocol::Map::new(),
        };
        // An allow-listed attribute survives.
        metric.attributes.insert("topic".into(), "tokens".into());
        // A non-allow-listed attribute carrying free-form text is stripped.
        metric
            .attributes
            .insert("raw_query".into(), "how do I mint NIGHT".into());
        // A secret anywhere is redacted.
        metric
            .attributes
            .insert("outcome".into(), format!("err {secret}").into());

        let scrubbed = scrub_metric(metric, &[secret.to_owned()]).expect("metric kept");
        assert!(scrubbed.attributes.contains_key("topic"));
        assert!(!scrubbed.attributes.contains_key("raw_query"), "unlisted key must be dropped");
        let json = serde_json::to_string(&scrubbed).expect("serialize");
        assert!(!json.contains(secret));
        assert!(json.contains("[REDACTED]"), "redaction marker must be present: {json}");
    }

    #[test]
    fn scrub_metric_fails_closed_when_redacted_metric_cannot_reparse() {
        // The `scrub_metric` counterpart to
        // `scrub_log_fails_closed_when_redacted_log_cannot_reparse` /
        // `scrub_fails_closed_when_redacted_event_cannot_reparse` (issue #166): if
        // redaction produces JSON that no longer round-trips into a `Metric`, the
        // metric MUST be dropped (`None`) — it must never be returned with the
        // pre-scrub secret still resolvable. This is the highest-consequence path:
        // a bug here ships a live secret.
        //
        // We trigger a reparse failure deterministically via `trace_id`, a
        // strictly-typed 16-byte id (`#[serde(try_from = "String", into =
        // "String")]`) that serializes to exactly 32 lowercase hex chars and
        // deserializes via `hex::decode_to_slice` into a fixed 16-byte buffer.
        // `Metric::trace_id` is non-optional, so it is set to `TraceId::default()`.
        // Registering that exact hex string as a "secret" makes redaction rewrite
        // `"trace_id":"<hex>"` into `"trace_id":"[REDACTED]"` — and `"[REDACTED]"`
        // is neither valid hex nor the right length, so `TraceId`'s
        // `FromStr`/`TryFrom<String>` impl errors and the whole `Metric` fails to
        // reparse.
        let trace_id = sentry::protocol::TraceId::default();
        let metric = sentry::protocol::Metric {
            r#type: sentry::protocol::MetricType::Counter,
            name: "search.requests".into(),
            value: 1.0,
            timestamp: std::time::SystemTime::now(),
            trace_id,
            span_id: None,
            unit: None,
            attributes: sentry::protocol::Map::new(),
        };

        // The exact serialized id, byte-for-byte what `scrub_metric` produces
        // internally, so the needle matches.
        let trace_id_hex = trace_id.to_string();
        assert_eq!(trace_id_hex.len(), 32, "TraceId must serialize to 32 hex chars");
        assert!(trace_id_hex.len() >= 8, "trace_id hex must clear the length filter");

        let result = scrub_metric(metric, &[trace_id_hex]);
        assert!(
            result.is_none(),
            "a redacted metric that cannot re-parse must be dropped (fail closed), \
             not returned unscrubbed"
        );
    }

    #[test]
    fn scrub_metric_drops_unlisted_attributes_even_with_no_secrets() {
        // The allow-list retain must run unconditionally, even when `secrets` is
        // empty and the fail-closed redaction path short-circuits via the
        // `relevant.is_empty()` early return. This guards that the privacy
        // allow-list is enforced independent of secret redaction.
        let mut metric = sentry::protocol::Metric {
            r#type: sentry::protocol::MetricType::Counter,
            name: "search.requests".into(),
            value: 1.0,
            timestamp: std::time::SystemTime::now(),
            trace_id: sentry::protocol::TraceId::default(),
            span_id: None,
            unit: None,
            attributes: sentry::protocol::Map::new(),
        };
        // An allow-listed attribute survives.
        metric.attributes.insert("topic".into(), "tokens".into());
        // A non-allow-listed attribute carrying free-form text is stripped.
        metric
            .attributes
            .insert("raw_query".into(), "how do I mint NIGHT".into());

        let scrubbed = scrub_metric(metric, &[]).expect("metric with no secrets is always kept");
        assert!(scrubbed.attributes.contains_key("topic"), "allow-listed key must survive");
        assert!(
            !scrubbed.attributes.contains_key("raw_query"),
            "unlisted key must be dropped even with no secrets configured"
        );
    }
}
