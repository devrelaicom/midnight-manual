//! HTTP route modules. Read endpoints land in this phase; write endpoints (with
//! auth wrappers) land in Phase 7 (US9).

pub mod health;
pub mod models;
pub mod search;
pub mod sources;
