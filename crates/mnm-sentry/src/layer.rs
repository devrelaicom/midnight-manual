//! The `sentry-tracing` layer + its level→EventFilter mapping.
//!
//! Widens today's ERROR-only capture: ERROR → Sentry event AND log;
//! WARN/INFO → log (+ breadcrumb for trace context); DEBUG/TRACE → ignored
//! (too verbose, and the widest leak surface). Panics are captured
//! independently by the panic integration.

use sentry::integrations::tracing::EventFilter;

/// Map a tracing level to what Sentry should do with the event.
#[must_use]
pub fn filter_for_level(level: tracing::Level) -> EventFilter {
    match level {
        tracing::Level::ERROR => EventFilter::Event | EventFilter::Log,
        tracing::Level::WARN | tracing::Level::INFO => EventFilter::Log | EventFilter::Breadcrumb,
        _ => EventFilter::Ignore,
    }
}

/// The layer to attach to a `tracing_subscriber` registry. Inert if Sentry
/// isn't initialized.
#[must_use]
pub fn tracing_layer<S>() -> sentry::integrations::tracing::SentryLayer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    sentry::integrations::tracing::layer().event_filter(|md| filter_for_level(*md.level()))
}

#[cfg(test)]
mod tests {
    use super::filter_for_level;
    use sentry::integrations::tracing::EventFilter;
    use tracing::Level;

    // `EventFilter` (bitflags, from `sentry-tracing` 0.48.4) is only
    // `#[derive(Debug, Clone, Copy)]` — it does NOT derive `PartialEq`, so
    // `assert_eq!` cannot compare `EventFilter` values directly (confirmed by
    // reading the vendored `sentry-tracing-0.48.4/src/layer/mod.rs` source).
    // Compare via `.bits()` (a `u32`, which is `PartialEq`) instead: this is
    // an exact-match check — it catches both missing bits and unwanted extra
    // bits, unlike `.contains()` which only proves a subset.
    #[test]
    fn error_is_event_and_log() {
        assert_eq!(
            filter_for_level(Level::ERROR).bits(),
            (EventFilter::Event | EventFilter::Log).bits()
        );
    }

    #[test]
    fn warn_info_are_log_and_breadcrumb() {
        for lvl in [Level::WARN, Level::INFO] {
            assert_eq!(
                filter_for_level(lvl).bits(),
                (EventFilter::Log | EventFilter::Breadcrumb).bits()
            );
        }
    }

    #[test]
    fn debug_trace_ignored() {
        assert_eq!(filter_for_level(Level::DEBUG).bits(), EventFilter::Ignore.bits());
        assert_eq!(filter_for_level(Level::TRACE).bits(), EventFilter::Ignore.bits());
    }
}
