//! Shared size limits used across the server (body cap) and CLI (upload batching).

/// Maximum HTTP request body the server accepts on ingest endpoints.
///
/// Also the figure the CLI sizes upload batches against. 25 MiB: enough
/// headroom for a size-aware upload batch (which targets ~85% of this) plus the
/// rough-estimate slack, while staying below a memory-exhaustion threshold on a
/// small machine.
pub const MAX_INGEST_BODY_BYTES: usize = 25 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::MAX_INGEST_BODY_BYTES;

    #[test]
    fn max_ingest_body_bytes_is_25_mib() {
        assert_eq!(MAX_INGEST_BODY_BYTES, 25 * 1024 * 1024);
    }
}
