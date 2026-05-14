//! `midnight-manual-server` — cloud HTTP API entrypoint.
//!
//! Loads config from env, connects to Postgres, runs migrations (unless opted
//! out via `MIDNIGHT_MANUAL_AUTO_MIGRATE=false` per D22), seeds the
//! `embedding_model` row if absent (FR-009.e), and starts the axum listener
//! on `0.0.0.0:8080` (or the port from `PORT`).

use std::net::SocketAddr;

use anyhow::Context as _;
use mn_server::{app, config::ServerConfig};
use tokio::signal;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Structured JSON logs from day one (FR-105). RUST_LOG override permitted.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cfg = ServerConfig::from_env().context("load server config")?;
    let pool = mn_store::pool::connect(&cfg.database_url)
        .await
        .context("connect to database")?;

    if cfg.auto_migrate {
        mn_store::pool::run_migrations(&pool)
            .await
            .context("run migrations")?;
    }

    // Resolve the active embedding model from the DB so handlers can compare
    // against a single source-of-truth instead of a hardcoded literal.
    // Fail-fast at boot if no row exists — that's a bad migration state and
    // every search would 409 anyway.
    let active = mn_store::entities::embedding_model::get_active(&pool)
        .await
        .context("resolve active embedding model (did migration 0006 run?)")?;
    let resolved_corpus_model = format!("{}@{}", active.name, active.revision);
    tracing::info!(corpus_model = %resolved_corpus_model, "resolved active embedding model");
    let mut cfg = cfg;
    cfg.corpus_model = Some(resolved_corpus_model);

    let app = app::build(pool, cfg.clone()).context("build app")?;
    let addr: SocketAddr = format!("0.0.0.0:{}", cfg.port)
        .parse()
        .context("parse listen address")?;

    tracing::info!(addr = %addr, "starting midnight-manual-server");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("bind listener")?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve")?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    tracing::info!("graceful shutdown initiated");
}
