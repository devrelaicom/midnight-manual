//! Tests for `mnm models migrate` — the model-migration driver.
//!
//! The core assertion is the **budget-stop loop** (`drive_migration`), exercised
//! with a stubbed ingest closure so no git clone or HTTP round-trip is needed.
//! The boundary semantics are:
//!
//! - Before starting each source, check the budget: if `tokens >= token_budget`
//!   or `docs >= max_docs`, STOP without starting it (overshoot is allowed
//!   *within* a source — the source that crosses the budget still completes, the
//!   next one is not started).
//! - On a mid-source `Err` (the 429/limit case — the pipeline already aborted
//!   rather than promoted), log + push the remaining sources (including the
//!   failed one) to `remaining` and STOP.

#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use midnight_manual::commands::models::{drive_migration, SourceOutcome, SourceRef};

fn src(slug: &str) -> SourceRef {
    SourceRef {
        slug: slug.to_owned(),
        origin_url: Some(format!("https://example.test/{slug}.git")),
    }
}

/// `token_budget = 50` < one source's 100 tokens: source 1 starts (boundary
/// check passes at tokens=0), source 2 does not (tokens=100 >= 50).
#[tokio::test]
async fn token_budget_stops_at_source_boundary() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_inner = Arc::clone(&calls);

    let summary =
        drive_migration(&[src("src-1"), src("src-2")], None, Some(50), move |s: &SourceRef| {
            let calls = Arc::clone(&calls_inner);
            let slug = s.slug.clone();
            async move {
                calls.lock().unwrap().push(slug);
                Ok(SourceOutcome {
                    docs: 10,
                    tokens: 100,
                    conflicts: 2,
                })
            }
        })
        .await;

    // Only source 1 was ingested.
    assert_eq!(*calls.lock().unwrap(), vec!["src-1".to_owned()]);
    assert_eq!(summary.migrated, vec!["src-1".to_owned()]);
    assert_eq!(summary.remaining, vec!["src-2".to_owned()]);
    assert_eq!(summary.docs, 10);
    assert_eq!(summary.tokens, 100);
    // The source that ran contributed its conflicts to the aggregate.
    assert_eq!(summary.conflicts, 2);
    // A budget stop is NOT an error: `remaining` is populated but
    // `aborted_on_error` stays `None`. This is exactly the distinction the
    // error-stop test below relies on.
    assert!(summary.aborted_on_error.is_none());
}

/// Pins the `>=` (not `>`) boundary: source 1 spends EXACTLY the budget
/// (`tokens == token_budget == 100`). Because the next-source check is
/// `tokens >= budget`, source 2 must NOT start even though we landed exactly
/// on the budget rather than overshooting it.
#[tokio::test]
async fn token_budget_exact_boundary_stops_next_source() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_inner = Arc::clone(&calls);

    let summary =
        drive_migration(&[src("src-1"), src("src-2")], None, Some(100), move |s: &SourceRef| {
            let calls = Arc::clone(&calls_inner);
            let slug = s.slug.clone();
            async move {
                calls.lock().unwrap().push(slug);
                // Source 1 lands EXACTLY on the budget (no overshoot).
                Ok(SourceOutcome {
                    docs: 0,
                    tokens: 100,
                    conflicts: 0,
                })
            }
        })
        .await;

    // Only source 1 ran; the exact-budget hit (100 >= 100) stops source 2.
    assert_eq!(*calls.lock().unwrap(), vec!["src-1".to_owned()]);
    assert_eq!(summary.migrated, vec!["src-1".to_owned()]);
    assert_eq!(summary.remaining, vec!["src-2".to_owned()]);
    assert_eq!(summary.tokens, 100);
    assert_eq!(summary.docs, 0);
}

/// `max_docs` equality variant of the `>=` boundary: source 1 produces EXACTLY
/// `max_docs` documents, so source 2 must not start (`docs >= max`).
#[tokio::test]
async fn max_docs_exact_boundary_stops_next_source() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_inner = Arc::clone(&calls);

    let summary =
        drive_migration(&[src("src-1"), src("src-2")], Some(10), None, move |s: &SourceRef| {
            let calls = Arc::clone(&calls_inner);
            let slug = s.slug.clone();
            async move {
                calls.lock().unwrap().push(slug);
                Ok(SourceOutcome {
                    docs: 10,
                    tokens: 0,
                    conflicts: 0,
                })
            }
        })
        .await;

    assert_eq!(*calls.lock().unwrap(), vec!["src-1".to_owned()]);
    assert_eq!(summary.migrated, vec!["src-1".to_owned()]);
    assert_eq!(summary.remaining, vec!["src-2".to_owned()]);
    assert_eq!(summary.docs, 10);
}

/// `max_docs = 5` < one source's 10 docs: source 1 starts (docs=0), source 2
/// does not (docs=10 >= 5).
#[tokio::test]
async fn max_docs_stops_at_source_boundary() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_inner = Arc::clone(&calls);

    let summary =
        drive_migration(&[src("src-1"), src("src-2")], Some(5), None, move |s: &SourceRef| {
            let calls = Arc::clone(&calls_inner);
            let slug = s.slug.clone();
            async move {
                calls.lock().unwrap().push(slug);
                Ok(SourceOutcome {
                    docs: 10,
                    tokens: 100,
                    conflicts: 0,
                })
            }
        })
        .await;

    assert_eq!(*calls.lock().unwrap(), vec!["src-1".to_owned()]);
    assert_eq!(summary.migrated, vec!["src-1".to_owned()]);
    assert_eq!(summary.remaining, vec!["src-2".to_owned()]);
    assert_eq!(summary.docs, 10);
}

/// No budget and no max-docs → every source is migrated.
#[tokio::test]
async fn no_budget_migrates_all_sources() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_inner = Arc::clone(&calls);

    let summary =
        drive_migration(&[src("src-1"), src("src-2")], None, None, move |s: &SourceRef| {
            let calls = Arc::clone(&calls_inner);
            let slug = s.slug.clone();
            async move {
                calls.lock().unwrap().push(slug);
                Ok(SourceOutcome {
                    docs: 10,
                    tokens: 100,
                    conflicts: 3,
                })
            }
        })
        .await;

    assert_eq!(*calls.lock().unwrap(), vec!["src-1".to_owned(), "src-2".to_owned()]);
    assert_eq!(summary.migrated, vec!["src-1".to_owned(), "src-2".to_owned()]);
    assert!(summary.remaining.is_empty());
    assert_eq!(summary.docs, 20);
    assert_eq!(summary.tokens, 200);
    // Conflicts accumulate across every migrated source (3 + 3).
    assert_eq!(summary.conflicts, 6);
}

/// A mid-source `Err` (the 429/limit case; the pipeline already aborted rather
/// than promoting) stops the run: the failed source and every untried source
/// after it land in `remaining`, and source 3 is never attempted.
#[tokio::test]
async fn mid_source_error_stops_and_records_remaining() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls_inner = Arc::clone(&calls);

    let summary = drive_migration(
        &[src("src-1"), src("src-2"), src("src-3")],
        None,
        None,
        move |s: &SourceRef| {
            let calls = Arc::clone(&calls_inner);
            let slug = s.slug.clone();
            async move {
                calls.lock().unwrap().push(slug.clone());
                if slug == "src-2" {
                    return Err(anyhow::anyhow!("429 Too Many Requests (token limit)"));
                }
                Ok(SourceOutcome {
                    docs: 10,
                    tokens: 100,
                    conflicts: 4,
                })
            }
        },
    )
    .await;

    // src-1 succeeded, src-2 was attempted (and aborted), src-3 never tried.
    assert_eq!(*calls.lock().unwrap(), vec!["src-1".to_owned(), "src-2".to_owned()]);
    assert_eq!(summary.migrated, vec!["src-1".to_owned()]);
    // The in-flight failed source AND the untried tail are "remaining".
    assert_eq!(summary.remaining, vec!["src-2".to_owned(), "src-3".to_owned()]);
    // Only src-1's counts accrued (the failed source's work was aborted).
    assert_eq!(summary.docs, 10);
    assert_eq!(summary.tokens, 100);
    assert_eq!(summary.conflicts, 4);
    // The genuine failure is recorded as `aborted_on_error` — this is what
    // `run_migrate` turns into a non-zero process exit, and what distinguishes
    // this stop from the budget stop (which leaves `aborted_on_error` None
    // while populating `remaining` identically).
    let abort = summary
        .aborted_on_error
        .as_ref()
        .expect("error stop records aborted_on_error");
    assert_eq!(abort.slug, "src-2");
    assert!(
        abort.error.contains("429"),
        "abort error preserves the failure detail: {}",
        abort.error
    );
}

/// Empty source list → empty summary, closure never called.
#[tokio::test]
async fn empty_sources_yields_empty_summary() {
    let summary = drive_migration(&[], None, None, |_s: &SourceRef| async move {
        Ok(SourceOutcome {
            docs: 0,
            tokens: 0,
            conflicts: 0,
        })
    })
    .await;
    assert!(summary.migrated.is_empty());
    assert!(summary.remaining.is_empty());
    assert_eq!(summary.docs, 0);
    assert_eq!(summary.tokens, 0);
    assert_eq!(summary.conflicts, 0);
}

// ── clap parse test for MigrateArgs ──────────────────────────────────────────

#[test]
fn migrate_args_parse_all_flags() {
    use clap::Parser as _;
    use midnight_manual::commands::models::MigrateArgs;

    #[derive(clap::Parser)]
    struct Wrap {
        #[command(flatten)]
        inner: MigrateArgs,
    }

    let w = Wrap::try_parse_from([
        "models-migrate",
        "--to",
        "voyage-code-4@1",
        "--source",
        "a,b",
        "--max-docs",
        "100",
        "--token-budget",
        "5000",
        "--manifests-dir",
        "/tmp/manifests",
    ])
    .unwrap();
    assert_eq!(w.inner.to.as_deref(), Some("voyage-code-4@1"));
    // value_delimiter splits the comma-separated list into two entries.
    assert_eq!(w.inner.source, vec!["a".to_owned(), "b".to_owned()]);
    assert_eq!(w.inner.max_docs, Some(100));
    assert_eq!(w.inner.token_budget, Some(5000));
    assert_eq!(w.inner.manifests_dir, std::path::PathBuf::from("/tmp/manifests"));
}

#[test]
fn migrate_args_defaults() {
    use clap::Parser as _;
    use midnight_manual::commands::models::MigrateArgs;

    #[derive(clap::Parser)]
    struct Wrap {
        #[command(flatten)]
        inner: MigrateArgs,
    }

    let w = Wrap::try_parse_from(["models-migrate"]).unwrap();
    assert!(w.inner.to.is_none());
    assert!(w.inner.source.is_empty());
    assert!(w.inner.max_docs.is_none());
    assert!(w.inner.token_budget.is_none());
    assert_eq!(w.inner.manifests_dir, std::path::PathBuf::from("manifests/midnight"));
}
