//! Integration test: migration 0009 creates `token_limit_override` and
//! `token_usage_snapshot` tables with the expected columns and constraints.

#![cfg(feature = "integration")]
mod common;

#[tokio::test]
async fn token_limit_tables_exist() {
    let h = common::boot().await;
    sqlx::query("INSERT INTO token_limit_override (subject_kind, subject, hourly, daily, expires_at, created_by) \
                 VALUES ('user', 'u1', 100, 1000, now() + interval '1 hour', 'admin')")
        .execute(&h.pool).await.expect("insert override");
    sqlx::query(
        "INSERT INTO token_usage_snapshot (subject_kind, subject, hour_epoch, tokens) \
                 VALUES ('ip', '203.0.113.1', 480000, 42)",
    )
    .execute(&h.pool)
    .await
    .expect("insert snapshot");
}
