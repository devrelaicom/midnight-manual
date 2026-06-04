//! Acceptance test for the Compact chunker (SC-028 surrogate, offline).
//!
//! Asserts module-based package detection and symbol-aware chunking against
//! real fixtures from compactp's own corpus. The full OZ `compact-contracts`
//! clone is a separate, network-dependent CI/manual step (see the design doc).
#![cfg(feature = "compact")]

use std::path::PathBuf;

use mn_content::chunk::{Chunker, ChunkerConfig};
use mn_content::code::compact::CompactChunker;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/compact")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn counter_has_symbol_chunks_and_no_package() {
    let src = fixture("counter.compact");
    let chunks = CompactChunker.chunk(&src, &ChunkerConfig::default()).unwrap();
    assert!(chunks.iter().all(|c| !c.fallback_used), "counter must parse cleanly");
    assert!(
        chunks.iter().any(|c| c.symbol_path.iter().any(|s| s.kind == "circuit" && s.name == "increment")),
        "expected [circuit increment]"
    );
    // module-less file → no package
    assert!(mn_content::detect_compact_package(&src).is_none());
}

#[test]
fn module_file_tags_package_and_nested_symbols() {
    let src = fixture("module_wpp.compact");
    // small budget forces per-item chunks so the module-nested path appears
    let cfg = ChunkerConfig { max_tokens: 24, ..ChunkerConfig::default() };
    let chunks = CompactChunker.chunk(&src, &cfg).unwrap();
    assert!(chunks.iter().all(|c| !c.fallback_used), "module_wpp must parse cleanly");

    // SC-028: exactly one top-level module M → compact/M package
    let pkg = mn_content::detect_compact_package(&src).expect("module M → package");
    assert_eq!(pkg.kind, "compact");
    assert_eq!(pkg.name, "M");

    // a chunk inside M carries the module prefix
    assert!(
        chunks.iter().any(|c| {
            c.symbol_path.iter().any(|s| s.kind == "module" && s.name == "M")
                && c.symbol_path.iter().any(|s| s.kind == "circuit")
        }),
        "expected a module-nested circuit chunk: {:?}",
        chunks.iter().map(|c| &c.symbol_path).collect::<Vec<_>>()
    );
    // a top-level circuit outside M has no module prefix
    assert!(
        chunks.iter().any(|c| {
            c.symbol_path.iter().any(|s| s.kind == "circuit" && s.name == "gary")
                && !c.symbol_path.iter().any(|s| s.kind == "module")
        }),
        "expected top-level [circuit gary] with no module prefix"
    );
}

#[test]
fn two_modules_leave_package_untagged() {
    let src = fixture("two_modules.compact");
    let chunks = CompactChunker.chunk(&src, &ChunkerConfig::default()).unwrap();
    assert!(chunks.iter().all(|c| !c.fallback_used), "two_modules must parse cleanly");
    assert!(mn_content::detect_compact_package(&src).is_none(), "multi-module → no package (P1)");
}
