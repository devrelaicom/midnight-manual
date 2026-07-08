//! Query-topic classifier storage: per-category centroids (cosine argmax lands in Task 10).
//!
//! "Category" is bound to `source.kind` (spec §9's single deferred binding) —
//! a low-cardinality, closed enum (`docs_site` / `code_repo` / `standalone` /
//! `mixed`), reached via `chunk -> source_version -> source`. Centroids are
//! recomputed per corpus embedding-model version whenever a version is
//! promoted (see `routes::admin_versions::promote_version`), and are always
//! L2-normalized so a later cosine-similarity argmax is a plain dot product.
//!
//! Uses runtime `sqlx::query` (this repo has no `.sqlx` offline cache and no
//! local database — the SQL is verified at runtime by CI's integration tests).

use sqlx::{PgPool, Row as _};
use uuid::Uuid;

/// Per-category centroids for one corpus embedding-model version.
pub struct Centroids {
    /// Category labels (`source.kind` values), sorted.
    pub labels: Vec<String>,
    /// L2-normalized mean embedding for each label, same order as `labels`.
    pub vectors: Vec<Vec<f32>>,
}

/// Recompute per-category centroids for `corpus_model_id` and replace the
/// stored rows. Returns the number of labels written.
///
/// Best-effort by design (see the promotion hook): a failure here must never
/// fail the corpus-version promotion that triggered it.
///
/// # Errors
///
/// Returns the underlying [`sqlx::Error`] (wrapped in `anyhow`) if the
/// aggregate query, the delete, or any insert fails.
pub async fn recompute_centroids(pool: &PgPool, corpus_model_id: Uuid) -> anyhow::Result<usize> {
    let rows = sqlx::query(
        "SELECT s.kind AS label, avg(c.embedding) AS centroid, count(*) AS n \
         FROM chunk c \
         JOIN source_version sv ON sv.id = c.source_version_id \
         JOIN source s ON s.id = sv.source_id \
         WHERE c.embedding_model_id = $1 AND c.embedding IS NOT NULL AND c.status = 'ready' \
         GROUP BY s.kind",
    )
    .bind(corpus_model_id)
    .fetch_all(pool)
    .await?;

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM topic_centroid WHERE corpus_model_id = $1")
        .bind(corpus_model_id)
        .execute(&mut *tx)
        .await?;

    let mut written = 0usize;
    for row in &rows {
        let label: String = row.try_get("label")?;
        let centroid: pgvector::Vector = row.try_get("centroid")?;
        let n: i64 = row.try_get("n")?;

        let mut v = centroid.to_vec();
        l2_normalize(&mut v);

        sqlx::query(
            "INSERT INTO topic_centroid (corpus_model_id, label, centroid, chunk_count) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(corpus_model_id)
        .bind(&label)
        .bind(pgvector::Vector::from(v))
        .bind(n)
        .execute(&mut *tx)
        .await?;
        written += 1;
    }

    tx.commit().await?;
    Ok(written)
}

/// Load the active model's centroids (already L2-normalized on write),
/// ordered by label.
///
/// # Errors
///
/// Returns the underlying [`sqlx::Error`] (wrapped in `anyhow`) on driver failure.
pub async fn load_centroids(pool: &PgPool, corpus_model_id: Uuid) -> anyhow::Result<Centroids> {
    let rows = sqlx::query(
        "SELECT label, centroid FROM topic_centroid WHERE corpus_model_id = $1 ORDER BY label",
    )
    .bind(corpus_model_id)
    .fetch_all(pool)
    .await?;

    let mut labels = Vec::with_capacity(rows.len());
    let mut vectors = Vec::with_capacity(rows.len());
    for row in &rows {
        labels.push(row.try_get::<String, _>("label")?);
        vectors.push(row.try_get::<pgvector::Vector, _>("centroid")?.to_vec());
    }
    Ok(Centroids { labels, vectors })
}

/// L2-normalize `v` in place. Leaves an all-zero vector unchanged (a
/// zero-norm centroid can't be normalized to unit length; it stays the
/// zero vector rather than dividing by zero).
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::l2_normalize;

    #[test]
    fn l2_normalize_unit_scales_vector_to_unit_length() {
        let mut v = vec![3.0_f32, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6, "expected 0.6, got {}", v[0]);
        assert!((v[1] - 0.8).abs() < 1e-6, "expected 0.8, got {}", v[1]);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "norm should be ~1.0, got {norm}");
    }

    #[test]
    fn l2_normalize_zero_vector_stays_zero() {
        let mut v = vec![0.0_f32, 0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }
}
