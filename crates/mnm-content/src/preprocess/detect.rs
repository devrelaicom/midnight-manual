//! `spdx::detection`-backed license text identification (Phase B).

use std::sync::LazyLock;

use spdx::detection::{Store, TextData};

use super::LicenseDetector;

/// Detection confidence threshold (spec: >= 0.9).
const THRESHOLD: f32 = 0.9;

/// Canonical header texts injected as `LicenseType::Header` variants when the
/// inline cache does not already provide headers for these common licenses.
/// (Only IDs whose headers materially differ from their full text need
/// entries; extend as calibration demands.)
///
/// Calibration finding (see task-9-report.md): against the plain inline
/// cache, `Apache-2.0` already had a `Header` variant that matched our
/// calibration notice at score 1.0 -- no fallback was load-bearing for it,
/// but it is kept per the brief (harmless robustness). `MIT` and
/// `BSD-3-Clause`, however, had no header variant: the closest the plain
/// cache could do for our calibration excerpts was 0.83 (correct license,
/// under threshold) and 0.66 (wrong license entirely) respectively -- so
/// those two fallbacks below are load-bearing, not optional. `MPL-2.0` and
/// `GPL-3.0-only` have no calibration test in this task; their fallbacks are
/// kept as untested but harmless robustness per the brief.
const HEADER_FALLBACKS: &[(&str, &str)] = &[
    ("Apache-2.0", include_str!("header_texts/apache-2.0.txt")),
    ("MIT", include_str!("header_texts/mit.txt")),
    ("BSD-3-Clause", include_str!("header_texts/bsd-3-clause.txt")),
    ("MPL-2.0", include_str!("header_texts/mpl-2.0.txt")),
    ("GPL-3.0-only", include_str!("header_texts/gpl-3.0.txt")),
];

/// Wraps a loaded [`Store`].
pub struct SpdxDetector {
    store: Store,
}

impl LicenseDetector for SpdxDetector {
    fn detect(&self, text: &str) -> Option<String> {
        let capped = &text[..floor_char_boundary(text, 8 * 1024)];
        let shaped = reflow_if_unbroken(capped, REFLOW_WIDTH);
        let m = self.store.analyze(&TextData::from(shaped.as_ref()));
        if m.score >= THRESHOLD && spdx::Expression::parse(m.name).is_ok() {
            Some(m.name.to_owned())
        } else {
            None
        }
    }
}

/// Largest index <= `max` that is a char boundary of `s`.
const fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Column width used to reflow single-line input before analysis; matches
/// typical hand-wrapped license prose.
const REFLOW_WIDTH: usize = 80;

/// Reflow `text` to `width`-column lines when it contains no line breaks at
/// all; otherwise return it unchanged.
///
/// `spdx::detection`'s aggressive preprocessing (`remove_copyright_statements`,
/// verified against spdx 0.13.4 source) special-cases "the very first line" via
/// `\A.*copyright.*$` -- a regex whose `.` does not cross `\n`. Real comment
/// blocks (see [`super::lexer::CommentBlock::text`]) always join their source
/// lines with real `\n`s, so that regex only ever consumes an actual first
/// line there. But text that arrives as one unbroken line -- a header/notice
/// that was joined or reflowed into a single paragraph -- has no `\n` at all,
/// so `\A.*copyright.*$` has nothing to stop it: if the word "copyright"
/// appears anywhere in the text, the WHOLE input matches and is deleted
/// before n-gram comparison, producing a `TextData` with an empty match set
/// and forcing every store comparison to score exactly 0 (`NgramSet::dice`
/// short-circuits on either side being empty). Reflowing to realistic
/// multi-line prose first -- content-preserving, only whitespace changes --
/// keeps that regex bounded to a real first line and avoids the failure mode.
fn reflow_if_unbroken(text: &str, width: usize) -> std::borrow::Cow<'_, str> {
    if text.contains('\n') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + text.len() / width.max(1) + 1);
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if col > 0 && col + 1 + word_len > width {
            out.push('\n');
            col = 0;
        } else if col > 0 {
            out.push(' ');
            col += 1;
        }
        out.push_str(word);
        col += word_len;
    }
    std::borrow::Cow::Owned(out)
}

static GLOBAL: LazyLock<Option<SpdxDetector>> = LazyLock::new(|| match Store::load_inline() {
    Ok(mut store) => {
        for (name, header) in HEADER_FALLBACKS {
            // Best-effort: a name absent from this build's SPDX list version
            // is skipped rather than failing the whole store load.
            let _ = store.add_variant(
                name,
                spdx::detection::LicenseType::Header,
                TextData::from(*header),
            );
        }
        Some(SpdxDetector { store })
    }
    Err(e) => {
        tracing::warn!(
            error = %e,
            "spdx inline detection cache failed to load; license text detection disabled for this process"
        );
        None
    }
});

/// The process-wide detector, or `None` when the cache failed to load
/// (degraded mode: heuristic head-stripping + SPDX-tag parsing still work).
#[must_use]
pub fn global() -> Option<&'static SpdxDetector> {
    GLOBAL.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preprocess::LicenseDetector;

    const APACHE_NOTICE: &str = "Licensed under the Apache License, Version 2.0 (the \"License\"); you may not use this file except in compliance with the License. You may obtain a copy of the License at http://www.apache.org/licenses/LICENSE-2.0 Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an \"AS IS\" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and limitations under the License.";

    const MIT_PERMISSION: &str = "Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the \"Software\"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions: The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software. THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.";

    const BSD3_HEADER: &str = "Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met: 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer. 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the documentation and/or other materials provided with the distribution. 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote products derived from this software without specific prior written permission.";

    #[test]
    fn detector_loads() {
        assert!(global().is_some(), "inline cache must load");
    }

    #[test]
    fn calibration_apache_header() {
        assert_eq!(global().unwrap().detect(APACHE_NOTICE).as_deref(), Some("Apache-2.0"));
    }

    #[test]
    fn calibration_mit() {
        assert_eq!(global().unwrap().detect(MIT_PERMISSION).as_deref(), Some("MIT"));
    }

    #[test]
    fn calibration_bsd3() {
        assert_eq!(global().unwrap().detect(BSD3_HEADER).as_deref(), Some("BSD-3-Clause"));
    }

    #[test]
    fn prose_mentioning_license_is_not_detected() {
        assert_eq!(
            global()
                .unwrap()
                .detect("This function is licensed to thrill and warranty-free."),
            None
        );
    }

    /// A complete, realistic MIT `LICENSE` file: a leading copyright line
    /// followed by the full canonical MIT license body, line-wrapped as it
    /// would appear in a real repository checkout. Distinct from
    /// `MIT_PERMISSION` above (a single-paragraph notice excerpt that omits
    /// the liability/warranty disclaimer's final sentence): this is the
    /// genuine multi-line full-file text, exercising the path where
    /// `reflow_if_unbroken` is a no-op (real `\n`s already present) and the
    /// store's `Original`/`Header` entries must do the matching work
    /// unassisted -- the generality case Task 10's walk-up resolver depends
    /// on.
    const FULL_MIT_LICENSE_FILE: &str = r#"Copyright (c) 2024 Example Holder

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"#;

    #[test]
    fn full_realistic_mit_license_file_detects() {
        assert_eq!(global().unwrap().detect(FULL_MIT_LICENSE_FILE).as_deref(), Some("MIT"));
    }
}
