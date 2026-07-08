//! Server-side Sentry instrumentation. The ONLY module allowed to attach query
//! content to Sentry, and only via the two sanctioned sinks (`search.topic`,
//! `search.query`). See the spec's two-sinks rule.

use mnm_auth::Role;

use crate::middleware::bearer::AuthContext;

/// Choose the Sentry `user.id`: a real admin `sub` for staff, a stable pseudonym
/// for everyone else, or `None` when no identity secret is configured.
#[must_use]
pub fn resolve_user_id(identity_secret: Option<&str>, sub: &str, is_admin: bool) -> Option<String> {
    if is_admin {
        Some(sub.to_owned())
    } else {
        mnm_sentry::helpers::pseudonymous_id(identity_secret, sub)
    }
}

/// Set the Sentry user for the current scope from the request's auth context.
pub fn set_request_identity(auth: Option<&AuthContext>, identity_secret: Option<&str>) {
    let Some(auth) = auth else { return };
    let is_admin = matches!(auth.role, Role::Admin);
    let Some(id) = resolve_user_id(identity_secret, &auth.sub, is_admin) else {
        return;
    };
    sentry::configure_scope(|scope| {
        scope.set_user(Some(sentry::User {
            id: Some(id),
            ..Default::default()
        }));
    });
}

/// Emit the per-search metrics (scalars + allow-listed attributes only).
pub fn record_search_metrics(outcome: &str, latency_ms: f64, topic: &str, code_mode: bool) {
    let attrs = [
        ("outcome", outcome.to_owned()),
        ("topic", topic.to_owned()),
        ("code_mode", code_mode.to_string()),
    ];
    mnm_sentry::helpers::record_count("search.requests", 1, &attrs);
    mnm_sentry::helpers::record_ms("search.latency", latency_ms, &attrs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_pseudonym_only_for_non_admin_when_secret_present() {
        // pseudonym derivation is delegated to mnm_sentry; here we assert the
        // selection logic: admins get their real sub, others get a pseudonym,
        // and with no secret non-admins get None.
        assert_eq!(
            resolve_user_id(Some("k"), "octocat", false),
            mnm_sentry::helpers::pseudonymous_id(Some("k"), "octocat")
        );
        assert_eq!(resolve_user_id(Some("k"), "aaron", true), Some("aaron".to_owned()));
        assert_eq!(resolve_user_id(None, "octocat", false), None);
    }
}
