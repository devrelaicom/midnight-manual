//! Shared testcontainers / live-database harness for midnight-manual-server integration tests.
//!
//! Behavior mirrors the harness in `crates/mnm-store/tests/common/mod.rs`: prefer
//! the `DATABASE_URL` env var (CI's `services: postgres`) and fall back to
//! testcontainers when not set. In `DATABASE_URL` mode each `boot()` runs
//! migrations against a fresh, uniquely-named schema (`search_path` falls back
//! to `public` for the shared pgvector/pgcrypto extensions) so parallel tests
//! don't collide on the shared `public` schema.

#![allow(
    dead_code,
    clippy::too_many_lines,
    clippy::large_enum_variant,
    clippy::doc_markdown
)]

use mnm_store::pool;
use sqlx::PgPool;

pub struct Harness {
    pub pool: PgPool,
    _container: ContainerHandle,
}

enum ContainerHandle {
    External,
    #[allow(dead_code)]
    Owned(testcontainers::ContainerAsync<PgVectorImage>),
}

pub async fn boot() -> Harness {
    if let Ok(url) = std::env::var("DATABASE_URL") {
        let pool = external_schema_pool(&url).await;
        pool::run_migrations(&pool)
            .await
            .expect("run migrations against DATABASE_URL test schema");
        return Harness {
            pool,
            _container: ContainerHandle::External,
        };
    }

    let container = testcontainers::runners::AsyncRunner::start(PgVectorImage)
        .await
        .expect("start pgvector container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("get container port");
    let url = format!("postgresql://postgres:dev@127.0.0.1:{port}/postgres");

    let pool = wait_for_pool(&url).await;
    pool::run_migrations(&pool)
        .await
        .expect("run migrations against testcontainers Postgres");

    Harness {
        pool,
        _container: ContainerHandle::Owned(container),
    }
}

/// Build a pool against a shared `DATABASE_URL`, isolated to a fresh,
/// uniquely-named schema.
///
/// Parallel integration tests share one Postgres in CI; without isolation they
/// collide on the `public` schema. Each `boot()` gets its own schema and a
/// `search_path` that falls back to `public` for the shared pgvector/pgcrypto
/// extensions, so the per-schema migrations resolve the `vector` type without
/// re-creating the extension.
async fn external_schema_pool(url: &str) -> PgPool {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sqlx::Executor as _;
    use std::time::Duration;

    let base: PgConnectOptions = url.parse().expect("parse DATABASE_URL");
    let schema = format!("t_{}", uuid::Uuid::new_v4().simple());

    // Setup connection: ensure the shared extensions exist in `public` and
    // create this test's schema. Extension creation is best-effort because
    // parallel setups race on `CREATE EXTENSION IF NOT EXISTS`.
    let setup = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(base.clone())
        .await
        .expect("connect setup to DATABASE_URL");
    let _ = setup.execute("CREATE EXTENSION IF NOT EXISTS vector").await;
    let _ = setup
        .execute("CREATE EXTENSION IF NOT EXISTS pgcrypto")
        .await;
    setup
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .expect("create per-test schema");
    setup.close().await;

    let opts = base.options([("search_path", format!("{schema},public"))]);
    PgPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(opts)
        .await
        .expect("connect to DATABASE_URL test schema")
}

async fn wait_for_pool(url: &str) -> PgPool {
    use std::time::Duration;
    let mut attempts = 0;
    loop {
        match pool::connect(url).await {
            Ok(p) => return p,
            Err(_) if attempts < 20 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => panic!("could not connect to testcontainers Postgres after retries: {e}"),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct PgVectorImage;

impl testcontainers::Image for PgVectorImage {
    fn name(&self) -> &'static str {
        "pgvector/pgvector"
    }
    fn tag(&self) -> &'static str {
        "pg16"
    }
    fn ready_conditions(&self) -> Vec<testcontainers::core::WaitFor> {
        vec![testcontainers::core::WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        )]
    }
    fn env_vars(
        &self,
    ) -> impl IntoIterator<
        Item = (impl Into<std::borrow::Cow<'_, str>>, impl Into<std::borrow::Cow<'_, str>>),
    > {
        [("POSTGRES_PASSWORD", "dev")]
    }
    fn expose_ports(&self) -> &[testcontainers::core::ContainerPort] {
        &[testcontainers::core::ContainerPort::Tcp(5432)]
    }
}
