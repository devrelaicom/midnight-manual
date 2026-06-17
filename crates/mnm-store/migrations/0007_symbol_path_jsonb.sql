-- Change chunk.symbol_path from text[] to jsonb to hold structured
-- {kind,name} segments. Greenfield: no code chunks exist yet (markdown uses
-- heading_path), so existing rows hold only the empty-array default.
ALTER TABLE chunk
    ALTER COLUMN symbol_path DROP DEFAULT;

ALTER TABLE chunk
    ALTER COLUMN symbol_path TYPE jsonb
    USING '[]'::jsonb;

ALTER TABLE chunk
    ALTER COLUMN symbol_path SET DEFAULT '[]'::jsonb;

ALTER TABLE chunk
    ALTER COLUMN symbol_path SET NOT NULL;

-- GIN index so symbol_path containment queries (@>) are fast,
-- e.g. find all chunks inside an `fn`: symbol_path @> '[{"kind":"fn"}]'.
CREATE INDEX idx_chunk_symbol_path ON chunk USING gin (symbol_path);
