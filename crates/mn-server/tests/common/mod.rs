//! Shared testcontainers / live-database harness for mn-server integration tests.
//!
//! Behavior mirrors the harness in `crates/mn-store/tests/common/mod.rs`: prefer
//! the `DATABASE_URL` env var (CI's `services: postgres`) and fall back to
//! testcontainers when not set.

#![allow(
    dead_code,
    clippy::too_many_lines,
    clippy::large_enum_variant,
    clippy::doc_markdown
)]

use mn_store::pool;
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
        let pool = pool::connect(&url).await.expect("connect to DATABASE_URL");
        pool::run_migrations(&pool)
            .await
            .expect("run migrations against DATABASE_URL");
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
