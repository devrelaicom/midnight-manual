//! `midnight-manual-server` — cloud HTTP API entrypoint.
//!
//! Loads config from env, connects to Postgres, runs migrations (unless opted
//! out via `MIDNIGHT_MANUAL_AUTO_MIGRATE=false` per D22), seeds the
//! `embedding_model` row if absent (FR-009.e), and starts the axum listener
//! on `0.0.0.0:8080` (or the port from `PORT`).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use mn_server::{app, config::ServerConfig, jobs};
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

    let app = app::build(pool.clone(), cfg.clone()).context("build app")?;
    let addr: SocketAddr = format!("0.0.0.0:{}", cfg.port)
        .parse()
        .context("parse listen address")?;

    tracing::info!(addr = %addr, "starting midnight-manual-server");

    // Background: telemetry retention sweep (FR-110 / SC-065). One task per
    // process — the JoinHandle stays alive for the duration of the server.
    let _sweep_handle =
        jobs::telemetry_sweep::spawn(pool.clone(), cfg.telemetry_raw_retention_days);

    // Background: embedder worker (Phase 11a / FR-038). Disabled in env when
    // the deployment has no GPU/CPU budget for ONNX; otherwise loads the
    // local model and starts polling for `embed_failed` chunks.
    let _embedder_handle = if cfg.embedder_enabled {
        let cache_env = mn_embedding::cache::StdEnv;
        let cache_dir = mn_embedding::cache::resolve(&cache_env).context(
            "could not resolve model cache dir (MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME)",
        )?;
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;
        let local = jobs::embedder::LocalEmbedder::load(cache_dir)
            .await
            .map_err(|e| anyhow::anyhow!("load local embedder: {e}"))?;
        Some(jobs::embedder::spawn(
            pool.clone(),
            Arc::new(local),
            active.id,
            Duration::from_millis(cfg.embedder_interval_ms),
            cfg.embedder_batch_size,
        ))
    } else {
        tracing::info!("embedder worker disabled (MIDNIGHT_MANUAL_EMBEDDER_ENABLED=false)");
        None
    };

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
