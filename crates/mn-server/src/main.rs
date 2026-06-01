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

    let rate_limiter = mn_server::ratelimit::RateLimiter::from_config(&cfg);
    let app = app::build_with_limiter(pool.clone(), cfg.clone(), rate_limiter.clone())
        .context("build app")?;
    let addr: SocketAddr = format!("0.0.0.0:{}", cfg.port)
        .parse()
        .context("parse listen address")?;

    tracing::info!(addr = %addr, "starting midnight-manual-server");

    // Background: telemetry retention sweep (FR-110 / SC-065). One task per
    // process — the JoinHandle stays alive for the duration of the server.
    let _sweep_handle =
        jobs::telemetry_sweep::spawn(pool.clone(), cfg.telemetry_raw_retention_days);

    // Background: full retention sweep (Phases 13/14/15). Three passes
    // per tick — retired sources, aged-out source_versions outside the
    // per-source `retention_count` window, and aborted ingest runs.
    // Cascades handle the children in all cases.
    let _source_retention_handle = if cfg.source_retirement_enabled {
        Some(jobs::source_retention::spawn(
            pool.clone(),
            cfg.source_retirement_grace_hours,
            cfg.source_version_sweep_grace_hours,
            cfg.abort_grace_hours,
            cfg.source_retirement_interval_minutes,
        ))
    } else {
        tracing::info!(
            "retention sweep disabled (MIDNIGHT_MANUAL_SOURCE_RETIREMENT_ENABLED=false)"
        );
        None
    };

    // Background: embedder worker (Phase 11a / FR-038). Disabled in env when
    // the deployment has no GPU/CPU budget for ONNX; otherwise polls for
    // `embed_failed` chunks and lazily loads the local model on the first
    // non-empty batch (so an idle server never holds the ~450 MB model).
    let _embedder_handle = if cfg.embedder_enabled {
        let cache_env = mn_embedding::cache::StdEnv;
        let cache_dir = mn_embedding::cache::resolve(&cache_env).context(
            "could not resolve model cache dir (MIDNIGHT_MANUAL_MODEL_CACHE_DIR or HOME)",
        )?;
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("create model cache dir at {}", cache_dir.display()))?;
        // Lazy: the model is NOT loaded here. The worker loads it on its first
        // non-empty batch, so an idle server never holds the ~450 MB model.
        let lazy = jobs::embedder::LazyEmbedder::local(cache_dir);
        Some(jobs::embedder::spawn(
            pool.clone(),
            Arc::new(lazy),
            active.id,
            Duration::from_millis(cfg.embedder_interval_ms),
            cfg.embedder_batch_size,
        ))
    } else {
        tracing::info!("embedder worker disabled (MIDNIGHT_MANUAL_EMBEDDER_ENABLED=false)");
        None
    };

    // Background: rate-limit override refresh + bucket reaper (Phase 17).
    // Only spawned when rate limiting is enabled.
    if let Some(limiter) = rate_limiter.clone() {
        if let Err(e) = limiter.refresh_overrides_now(&pool).await {
            tracing::warn!(error = %e, "initial rate-limit override load failed");
        }
        let refresh_pool = pool.clone();
        let refresh_secs = cfg.rate_limit_override_refresh_secs;
        let refresh_limiter = limiter.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(refresh_secs.max(1)));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                if let Err(e) = refresh_limiter.refresh_overrides_now(&refresh_pool).await {
                    tracing::warn!(error = %e, "rate-limit override refresh failed");
                }
            }
        });
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                limiter.reap(Duration::from_secs(300));
            }
        });
    }

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
