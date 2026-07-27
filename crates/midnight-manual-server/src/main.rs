//! `midnight-manual-server` — cloud HTTP API entrypoint.
//!
//! Loads config from env, connects to Postgres, runs migrations (unless opted
//! out via `MIDNIGHT_MANUAL_AUTO_MIGRATE=false` per D22), seeds the
//! `embedding_model` row if absent (FR-009.e), and starts the axum listener
//! on `0.0.0.0:8080` (or the port from `PORT`).

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context as _;
use midnight_manual_server::{app, config::ServerConfig, jobs};
use tokio::signal;
use tracing_subscriber::EnvFilter;

// `main` is the boot/wiring entrypoint: it threads config, DB, the resolved
// corpus model, and several background tasks. Splitting it would scatter the
// boot sequence across helpers for no readability gain, so the line lint is
// allowed here (consistent with the server-scaffold lint allowances in lib.rs).
#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::Layer as _;

    // Opt-in Sentry error reporting (mnm-sentry). Init BEFORE the subscriber so
    // the sentry-tracing layer can attach, and hold the guard for all of `main`
    // so buffered events flush on shutdown. Disabled by default — see the gate
    // in `mnm_sentry`. We gather secrets from raw env (config isn't parsed yet,
    // which also avoids ordering issues): every value below is redacted from
    // outgoing events. The server has no admin user, so `admin_present = true`
    // (the admin gate is client-only) and `admin_user_id = None`.
    let env = mnm_core::config::StdEnv;
    // Single source of truth for the four Sentry pillars — mirrors what
    // `ServerConfig::from_env` will resolve later, without duplicating the
    // defaults here (config isn't parsed yet at this point in boot).
    let sentry_cfg =
        midnight_manual_server::config::SentryRuntime::from_env_with(|k| std::env::var(k).ok());
    let sentry_guard = {
        let mut secrets = Vec::new();
        for name in [
            "DATABASE_URL",
            "VOYAGE_API_KEY",
            "MIDNIGHT_MANUAL_JWT_SECRET",
            "MIDNIGHT_MANUAL_GITHUB_OAUTH_CLIENT_SECRET",
            "MIDNIGHT_MANUAL_USER_STORE",
            "MIDNIGHT_MANUAL_INJECTION_HF_TOKEN",
            mnm_sentry::KEY_ENV,
        ] {
            if let Some(v) = std::env::var(name).ok().filter(|s| !s.is_empty()) {
                secrets.push(v);
            }
        }
        if let Some(id_secret) = sentry_cfg.identity_secret.clone() {
            secrets.push(id_secret);
        }
        mnm_sentry::init(
            &env,
            mnm_sentry::InitOptions {
                admin_present: true,
                release: env!("CARGO_PKG_VERSION"),
                default_environment: "production",
                admin_user_id: None,
                secrets,
                enable_logs: sentry_cfg.enable_logs,
                enable_metrics: sentry_cfg.enable_metrics,
                enable_traces: sentry_cfg.enable_traces,
                traces_sample_rate: sentry_cfg.traces_sample_rate,
                surface: "server",
            },
        )
    };

    // Structured JSON logs from day one (FR-105). RUST_LOG override permitted.
    // The EnvFilter is a per-layer filter on the fmt layer (not a global registry
    // filter), so it gates only the JSON logs; the sentry layer stays unfiltered
    // and applies its own level mapping (ERROR -> event + log, WARN/INFO -> log)
    // regardless of RUST_LOG — consistent with the CLI. The sentry layer is
    // attached only when Sentry initialized (inert otherwise); `Option<Layer>`
    // is itself a `Layer`.
    let sentry_layer = sentry_guard.as_ref().map(|_| mnm_sentry::tracing_layer());
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_target(false)
                .with_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                ),
        )
        .with(sentry_layer)
        .init();

    let cfg = ServerConfig::from_env().context("load server config")?;
    let pool = mnm_store::pool::connect(&cfg.database_url)
        .await
        .context("connect to database")?;

    if cfg.auto_migrate {
        mnm_store::pool::run_migrations(&pool)
            .await
            .context("run migrations")?;
    }

    // Resolve the active embedding model from the DB so handlers can compare
    // against a single source-of-truth instead of a hardcoded literal.
    // Fail-fast at boot if no row exists — that's a bad migration state and
    // every search would 409 anyway.
    let active = mnm_store::entities::embedding_model::get_active(&pool)
        .await
        .context("resolve active embedding model (did migration 0006 run?)")?;
    let resolved_corpus_model = format!("{}@{}", active.name, active.revision);
    tracing::info!(corpus_model = %resolved_corpus_model, "resolved active embedding model");

    // Re-resolvable corpus-model handle for AppState (Task 3.2). Reuse the
    // already-resolved `active` row rather than issuing a second query.
    let corpus = midnight_manual_server::corpus_model::CorpusModel {
        wire: resolved_corpus_model,
        name: active.name.clone(),
        id: active.id,
        dim: usize::try_from(active.dim)
            .context("active embedding model dim out of range for usize")?,
    };

    let rate_limiter = midnight_manual_server::ratelimit::RateLimiter::from_config(&cfg);
    if rate_limiter.is_none() {
        // Opting out is legitimate (e.g. a limiter upstream), but it must
        // never be silent — an unmetered server should be a decision, not a
        // discovery.
        tracing::warn!(
            "request rate limiting DISABLED (MIDNIGHT_MANUAL_RATE_LIMIT_ENABLED is falsy); \
             every caller has unmetered request throughput"
        );
    }
    // Token-usage limiter (tiered embedding-token ceilings). Kept in a binding
    // so Task 4.8's snapshot/reaper job can share this exact instance.
    let token_limiter = midnight_manual_server::tokenlimit::TokenUsageLimiter::from_config(&cfg);

    // Config-pinned code-embedding model, resolved against the registry. An
    // unresolved model degrades (code_mode searches 503) rather than failing
    // boot — a corpus ingested without code embeddings is still serviceable.
    let code = match midnight_manual_server::code_model::resolve(&pool, &cfg.code_model_wire).await
    {
        Ok(cm) => {
            tracing::info!(code_model = %cm.wire, "resolved code embedding model");
            Some(cm)
        }
        Err(e) => {
            tracing::warn!(error = %e, "code model unresolved; code_mode searches will 503");
            None
        }
    };

    // Server-side Voyage embedders for POST /v1/embeddings (None when no key).
    // Cross-element drift fix: each embedder's model NAME/dim comes from the SAME
    // resolved corpus/code model whose wire id the route stamps on the response —
    // NOT from `cfg.voyage_model` / `cfg.voyage_context_model`. The
    // MIDNIGHT_MANUAL_VOYAGE_MODEL / _CONTEXT_MODEL env vars survive only as an
    // explicit override, warn-logged when they disagree with the registry. See
    // `app::resolved_embedders`.
    //
    // BINDING IS PINNED AT BOOT: these embedders are built once, here, from the
    // corpus/code model resolved at boot. They are NOT re-resolved when an ingest
    // finalize promotes a different model at runtime (`corpus_model::refresh`).
    // After such a swap the proxy keeps computing with the boot model while the
    // wire-id label moves to the new one — a RESTART re-aligns them.
    // `corpus_model::refresh` fails loud (warns) when that happens.
    let (voyage, voyage_ctx) =
        midnight_manual_server::app::resolved_embedders(&cfg, &corpus, code.as_ref());

    // Captured before `corpus` moves into the `corpus_model` handle below —
    // `Uuid` is `Copy` so this is a cheap read, not a borrow of `corpus`.
    let corpus_model_id = corpus.id;
    let corpus_model = std::sync::Arc::new(std::sync::RwLock::new(Some(corpus)));
    let code_model: midnight_manual_server::code_model::Shared =
        std::sync::Arc::new(std::sync::RwLock::new(code));

    // Best-effort load of the active model's topic centroids (Task 10). `None`
    // on failure — the classifier (wired in a later task) then treats every
    // query as unbounded/`"other"` rather than failing boot.
    let topic_centroids = std::sync::Arc::new(std::sync::RwLock::new(
        midnight_manual_server::observability::topic::load_centroids(&pool, corpus_model_id)
            .await
            .ok(),
    ));

    let app = app::build_with_limiter(
        pool.clone(),
        cfg.clone(),
        rate_limiter.clone(),
        corpus_model,
        token_limiter.clone(),
        voyage,
        voyage_ctx,
        code_model,
        topic_centroids,
    )
    .context("build app")?;
    let addr: SocketAddr = format!("0.0.0.0:{}", cfg.port)
        .parse()
        .context("parse listen address")?;

    tracing::info!(addr = %addr, "starting midnight-manual-server");

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

    // The corpus is embedded with VoyageAI (client-side BYOK or the server's
    // POST /v1/embeddings proxy) and the CLI always uploads ready vectors, so
    // there is no longer a server-side embedder worker to backfill
    // `embed_failed` chunks. `active` above is retained solely to construct the
    // resolved `CorpusModel` (id/dim/wire) for AppState.

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

    // Background: token-usage durability + token-limit override refresh
    // (Task 4.8). Unlike rate limiting, token accounting has no disable switch,
    // so this always runs.
    //
    // 1. Seed in-memory per-hour buckets from the snapshot so accounting
    //    survives a restart within the rolling day window.
    let boot_now = time::OffsetDateTime::now_utc().unix_timestamp();
    if let Err(e) = token_limiter.load_from_db(&pool, boot_now).await {
        tracing::warn!(error = %e, "token usage snapshot load failed");
    }
    // 2. Load the override cache once before serving (so the first request sees
    //    operator-configured ceilings).
    if let Err(e) = token_limiter.refresh_overrides_now(&pool).await {
        tracing::warn!(error = %e, "initial token-limit override load failed");
    }
    // 3. Periodic override refresh (same cadence as the rate-limit refresh).
    {
        let refresh_pool = pool.clone();
        let refresh_secs = cfg.rate_limit_override_refresh_secs;
        let refresh_limiter = token_limiter.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(refresh_secs.max(1)));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                if let Err(e) = refresh_limiter.refresh_overrides_now(&refresh_pool).await {
                    tracing::warn!(error = %e, "token-limit override refresh failed");
                }
            }
        });
    }
    // 4. Periodic usage snapshot + idle-subject eviction + stale-row prune.
    let _token_snapshot_handle = jobs::token_usage_snapshot::spawn(
        pool.clone(),
        token_limiter.clone(),
        cfg.token_snapshot_secs,
    );

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("bind listener")?;
    // `into_make_service_with_connect_info` surfaces the socket peer address as
    // a `ConnectInfo<SocketAddr>` extension. The rate-limit middleware and the
    // token-limiter routes key off it when the trusted proxy header is absent,
    // instead of a spoofable `X-Forwarded-For` (issue #176 L15).
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
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
