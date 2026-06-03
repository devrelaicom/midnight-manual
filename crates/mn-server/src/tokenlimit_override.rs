//! Parsed embedding token-limit overrides + CIDR/user matching.
//!
//! Mirrors `ratelimit`'s override cache but keyed by subject_kind (CIDR block
//! or user id) and carrying hourly+daily ceilings instead of an rps.
use crate::ratelimit::ip_in;
use mn_core::types::TokenLimitOverride;
use std::net::IpAddr;
use time::OffsetDateTime;

/// A parsed, in-memory override row.
#[derive(Debug, Clone)]
pub enum Parsed {
    /// A CIDR-block override: applies to all IPs within `net/prefix`.
    Cidr {
        /// Network address (host bits already zeroed).
        net: IpAddr,
        /// Prefix length in bits.
        prefix: u8,
        /// Original `addr/prefix` string (used as the cache key).
        raw: String,
        /// Per-hour embedding token ceiling.
        hourly: u64,
        /// Per-day embedding token ceiling.
        daily: u64,
        /// When the row was created (tie-breaker for overlapping CIDRs).
        created_at: OffsetDateTime,
    },
    /// A user-id exact-match override.
    User {
        /// JWT `sub` this override applies to.
        id: String,
        /// Per-hour embedding token ceiling.
        hourly: u64,
        /// Per-day embedding token ceiling.
        daily: u64,
    },
}

impl Parsed {
    /// Test/constructor helper for a user override.
    #[must_use]
    pub fn user(id: &str, hourly: u64, daily: u64) -> Self {
        Self::User {
            id: id.to_owned(),
            hourly,
            daily,
        }
    }
}

/// Parse a stored override row into a [`Parsed`]. Returns `None` for a CIDR
/// row whose `subject` is not a parseable `addr/prefix`, or any row whose
/// hourly/daily is negative (can't happen given the DB CHECK, but be safe).
#[must_use]
pub fn parse_row(row: TokenLimitOverride) -> Option<Parsed> {
    let hourly = u64::try_from(row.hourly).ok()?;
    let daily = u64::try_from(row.daily).ok()?;
    match row.subject_kind.as_str() {
        "user" => Some(Parsed::User { id: row.subject, hourly, daily }),
        "cidr" => {
            let (net_s, prefix_s) = row
                .subject
                .split_once('/')
                .unwrap_or((row.subject.as_str(), ""));
            let net: IpAddr = net_s.parse().ok()?;
            let prefix = if prefix_s.is_empty() {
                if net.is_ipv4() {
                    32
                } else {
                    128
                }
            } else {
                prefix_s.parse().ok()?
            };
            Some(Parsed::Cidr {
                net,
                prefix,
                raw: row.subject,
                hourly,
                daily,
                created_at: row.created_at,
            })
        }
        _ => None,
    }
}

/// Best CIDR override for `ip`: longest prefix, ties broken by newest
/// `created_at`. Returns `(hourly, daily)`.
#[must_use]
pub fn match_cidr(overrides: &[Parsed], ip: IpAddr) -> Option<(u64, u64)> {
    overrides
        .iter()
        .filter_map(|o| match o {
            Parsed::Cidr {
                net,
                prefix,
                hourly,
                daily,
                created_at,
                ..
            } if ip_in(*net, *prefix, ip) => Some((*prefix, *created_at, *hourly, *daily)),
            _ => None,
        })
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))
        .map(|(_, _, h, d)| (h, d))
}

/// Exact user override by id. Returns `(hourly, daily)`.
#[must_use]
pub fn match_user(overrides: &[Parsed], id: &str) -> Option<(u64, u64)> {
    overrides.iter().find_map(|o| match o {
        Parsed::User { id: oid, hourly, daily } if oid == id => Some((*hourly, *daily)),
        _ => None,
    })
}
