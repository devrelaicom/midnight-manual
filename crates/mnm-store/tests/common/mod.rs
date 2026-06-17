//! Shared testcontainers / live-database harness for mnm-store integration tests.
//!
//! Behavior:
//! - If `DATABASE_URL` is set in the environment (as in CI's `integration` job),
//!   connect to that database and run migrations against a fresh, uniquely-
//!   named schema per `boot()` call (`search_path` falls back to `public` for
//!   the shared pgvector/pgcrypto extensions). This isolates parallel tests
//!   that would otherwise collide on the shared `public` schema. The schema is
//!   left in place for the ephemeral CI database to reclaim when the job ends.
//! - Otherwise, spin up an ephemeral `pgvector/pgvector:pg16` container via
//!   testcontainers; works on any developer machine with Docker running. Each
//!   `boot()` gets its own container, so that path is already isolated.
//!
//! Both paths return a `PgPool` connected to a fully-migrated database.

#![allow(
    dead_code, // each integration test pulls a subset of helpers
    clippy::too_many_lines, // test setup is verbose by design
    clippy::large_enum_variant, // ContainerHandle holds either nothing or a heavy testcontainers struct; we never have many of these
)]

use mnm_store::pool;
use sqlx::PgPool;

/// Holds either a borrowed `DATABASE_URL` connection or an owned testcontainer
/// so the container's lifetime survives until the test completes.
pub struct Harness {
    pub pool: PgPool,
    _container: ContainerHandle,
}

enum ContainerHandle {
    /// Live `DATABASE_URL` mode — no container to own.
    External,
    /// Owned testcontainers Postgres node.
    #[allow(dead_code)]
    Owned(testcontainers::ContainerAsync<PgVectorImage>),
}

/// Boot the test harness — migrations are run before the pool is returned.
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
/// collide on the `public` schema (duplicate slugs, cross-test row counts, the
/// `uniq_source_version_active` partial index, …). Each `boot()` gets its own
/// schema and a `search_path` that falls back to `public` for the shared
/// pgvector/pgcrypto extensions, so the per-schema migrations resolve the
/// `vector` type without re-creating the extension.
async fn external_schema_pool(url: &str) -> PgPool {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sqlx::Executor as _;
    use std::time::Duration;

    let base: PgConnectOptions = url.parse().expect("parse DATABASE_URL");
    let schema = format!("t_{}", uuid::Uuid::new_v4().simple());

    // Setup connection: ensure the shared extensions exist in `public` (so each
    // schema resolves the `vector` type via the search_path fallback) and
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

/// pgvector/pgvector:pg16 image definition for testcontainers.
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
