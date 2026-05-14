//! Background jobs spawned at server boot.
//!
//! Each job is a `tokio::spawn`-able async function that loops on a
//! `tokio::time::interval`. The orchestrator in `main.rs` owns the handles
//! and the graceful-shutdown wiring; the jobs themselves are stateless
//! between ticks.

pub mod embedder;
pub mod telemetry_sweep;
