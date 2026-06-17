//! Build script for `mnm-content`.
//!
//! Compiles the vendored tree-sitter Scheme grammar (`vendor/tree-sitter-scheme`)
//! into a static lib exposing the `tree_sitter_scheme` C symbol, but only when the
//! `scheme` cargo feature is enabled.

fn main() {
    // Cargo sets `CARGO_FEATURE_<NAME>` for each enabled feature when running the
    // build script. `#[cfg(feature = "scheme")]` does NOT work here — build
    // scripts are compiled without the package's feature cfgs, so a `#[cfg]`
    // guard would always compile out, `parser.c` would never build, and the
    // `scheme` module's reference to `tree_sitter_scheme` would fail to link.
    if std::env::var_os("CARGO_FEATURE_SCHEME").is_some() {
        let dir = std::path::Path::new("vendor/tree-sitter-scheme/src");
        let mut build = cc::Build::new();
        build.include(dir).file(dir.join("parser.c"));
        let scanner = dir.join("scanner.c");
        if scanner.exists() {
            build.file(scanner);
        }
        build.warnings(false).compile("tree_sitter_scheme");
        println!("cargo:rerun-if-changed=vendor/tree-sitter-scheme/src");
    }
}
