-- Per-category query-topic centroids, keyed by corpus embedding-model version.
-- Recomputed on promotion; the topic classifier reads the active model's rows.
CREATE TABLE IF NOT EXISTS topic_centroid (
    corpus_model_id uuid        NOT NULL REFERENCES embedding_model(id) ON DELETE CASCADE,
    label           text        NOT NULL,
    centroid        vector      NOT NULL,   -- L2-normalized mean of the category's chunk embeddings
    chunk_count     bigint      NOT NULL,
    computed_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (corpus_model_id, label)
);
