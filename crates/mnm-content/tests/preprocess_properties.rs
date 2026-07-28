//! Preprocess invariants (spec §Testing): idempotency and no-panic on
//! arbitrary input, including string-literal adversaries.

use std::path::Path;

use mnm_content::preprocess::preprocess;
use mnm_core::types::DocumentKind;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn never_panics_and_idempotent_code(body in "[\\P{C}\\n]{0,2000}") {
        let once = preprocess(DocumentKind::Code, Path::new("f.rs"), &body, None);
        let twice = preprocess(DocumentKind::Code, Path::new("f.rs"), &once.body, None);
        prop_assert_eq!(&once.body, &twice.body);
    }

    #[test]
    fn never_panics_and_idempotent_markdown(body in "[\\P{C}\\n]{0,2000}") {
        let once = preprocess(DocumentKind::Markdown, Path::new("f.mdx"), &body, None);
        let twice = preprocess(DocumentKind::Markdown, Path::new("f.mdx"), &once.body, None);
        prop_assert_eq!(&once.body, &twice.body);
    }
}

#[test]
fn string_literal_adversaries_survive() {
    // Each case pairs a fake-license payload sitting inside a string/template/
    // docstring/regex literal with the exact at-risk substring that a naive,
    // string-unaware comment stripper would delete. The line-oriented lexer
    // (spec's accepted two-factor risk, see lexer.rs) classifies whole lines
    // by their leading token, so it can be fooled into treating some of these
    // lines as comment-only -- but with `None` detector and no SPDX tag, the
    // mid-file rule (rule 2) still requires a tag or a confident detection
    // before stripping, so every payload below must survive verbatim:
    //   - rust raw-string / js regex: the fake comment shares a line with
    //     real code, so the line is never classified comment-only at all.
    //   - ts template literal / python docstring: the fake comment is alone
    //     on its own line (so it IS lexed as a comment block), but it's
    //     mid-file with no SPDX tag and no detector, so rule 2 spares it.
    let cases: &[(&str, &str, &str)] = &[
        ("t.rs", "let s = r#\"// Copyright fake\"#;\nfn a() {}\n", "Copyright fake"),
        (
            "t.ts",
            "const s = `\n// Licensed under nothing\n`;\nconsole.log(s);\n",
            "Licensed under nothing",
        ),
        (
            "t.py",
            "s = \"\"\"\n# Copyright in a docstring\n\"\"\"\nx = 1\n",
            "Copyright in a docstring",
        ),
        ("t.js", "const re = /\\/\\/ license/;\nuse(re);\n", "\\/\\/ license"),
    ];
    for (path, body, risky) in cases.iter().copied() {
        let out = preprocess(DocumentKind::Code, Path::new(path), body, None);
        assert!(
            out.body.contains(risky),
            "{path}: lost at-risk payload {risky:?} -- full body was {body:?}, stripped to {:?}",
            out.body
        );
    }
}
