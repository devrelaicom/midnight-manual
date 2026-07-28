//! Preprocess invariants (spec §Testing): idempotency and no-panic on
//! arbitrary input, including string-literal adversaries.

use std::path::Path;

use mnm_content::preprocess::preprocess;
use mnm_core::types::DocumentKind;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn never_panics_and_idempotent_code(body in "\\PC{0,2000}") {
        let once = preprocess(DocumentKind::Code, Path::new("f.rs"), &body, None);
        let twice = preprocess(DocumentKind::Code, Path::new("f.rs"), &once.body, None);
        prop_assert_eq!(&once.body, &twice.body);
    }

    #[test]
    fn never_panics_and_idempotent_markdown(body in "\\PC{0,2000}") {
        let once = preprocess(DocumentKind::Markdown, Path::new("f.mdx"), &body, None);
        let twice = preprocess(DocumentKind::Markdown, Path::new("f.mdx"), &once.body, None);
        prop_assert_eq!(&once.body, &twice.body);
    }
}

#[test]
fn string_literal_adversaries_survive() {
    let cases: &[(&str, &str)] = &[
        ("t.rs", "let s = r#\"// Copyright fake\"#;\nfn a() {}\n"),
        ("t.ts", "const s = `\n// Licensed under nothing\n`;\nconsole.log(s);\n"),
        ("t.py", "s = \"\"\"\n# Copyright in a docstring\n\"\"\"\nx = 1\n"),
        ("t.js", "const re = /\\/\\/ license/;\nuse(re);\n"),
    ];
    for (path, body) in cases {
        let out = preprocess(DocumentKind::Code, Path::new(path), body, None);
        // Two-factor safety: without a detector, mid-file strips need an SPDX
        // tag — none present, so ALL these bodies must survive semantically:
        // every non-comment line is still present.
        for line in body
            .lines()
            .filter(|l| !l.trim_start().starts_with(['#', '/']))
        {
            assert!(out.body.contains(line.trim_end()), "{path}: lost {line:?}");
        }
    }
}
