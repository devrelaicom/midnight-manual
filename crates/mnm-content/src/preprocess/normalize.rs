//! Final normalization pass (spec rules 9-11).
//!
//! Rule 9: trim trailing whitespace per line. Rule 10: collapse runs of 2+
//! blank lines down to exactly one. Rule 11: trim leading/trailing blank
//! lines from the whole file. Operates line-wise over `split('\n')`, so it
//! never slices on anything but a `\n` boundary and is safe on any valid
//! UTF-8 input.
//!
//! Must be a fixpoint: `normalize(normalize(x).0).0 == normalize(x).0`. The
//! orchestrator's idempotency test depends on this holding for every
//! document kind, since `preprocess` always runs this pass last.

/// Apply rules 9-11 to `body`.
///
/// Returns the normalized text and the number of bytes the pass removed
/// (saturating at zero; a body that gains a trailing newline it lacked is
/// not reported as a negative removal).
#[must_use]
pub fn normalize(body: &str) -> (String, usize) {
    let mut out = String::with_capacity(body.len());
    let mut blank_run = 0usize;
    for line in body.split('\n') {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            continue;
        }
        if !out.is_empty() {
            // Re-emit at most one blank line between content lines.
            out.push('\n');
            if blank_run > 0 {
                out.push('\n');
            }
        }
        blank_run = 0;
        out.push_str(trimmed);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    let removed = body.len().saturating_sub(out.len());
    (out, removed)
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn trims_collapses_and_edge_trims() {
        let (out, removed) = normalize("\n\nline one   \n\n\n\nline two\t\n\n\n");
        assert_eq!(out, "line one\n\nline two\n");
        assert!(removed > 0);
    }

    #[test]
    fn empty_input_stays_empty() {
        let (out, removed) = normalize("");
        assert_eq!(out, "");
        assert_eq!(removed, 0);
    }

    #[test]
    fn is_a_fixpoint() {
        for body in [
            "\n\nline one   \n\n\n\nline two\t\n\n\n",
            "plain\n\n\ntext",
            "already\n\nnormal\n",
            "",
            "   \n   \n",
            "single line no newline",
        ] {
            let (once, _) = normalize(body);
            let (twice, _) = normalize(&once);
            assert_eq!(once, twice, "not a fixpoint for {body:?}");
        }
    }
}
