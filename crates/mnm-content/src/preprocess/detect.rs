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
        // 16 KiB, not 8: the full Apache-2.0 license is ~11 KiB, and truncating
        // it drops enough of the document that Sørensen-Dice falls below the 0.9
        // threshold (~0.84), so a real Apache-2.0 LICENSE file would go
        // undetected. 16 KiB covers Apache-2.0/MIT/BSD in full; larger licenses
        // (GPL) are rare in this corpus and resolve via the manifest field.
        let capped = &text[..floor_char_boundary(text, 16 * 1024)];
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
            if let Err(e) = store.add_variant(
                name,
                spdx::detection::LicenseType::Header,
                TextData::from(*header),
            ) {
                tracing::debug!(
                    license = name,
                    error = %e,
                    "header fallback variant not added (license id absent from this SPDX list version)"
                );
            }
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

    /// A complete, realistic Apache-2.0 `LICENSE` file: the full "Apache
    /// License Version 2.0, January 2004" document (definitions through
    /// Section 9, the terms-and-conditions close, and the appendix), as it
    /// reads in a real repository checkout -- not the short notice excerpt
    /// (`APACHE_NOTICE` / `header_texts/apache-2.0.txt`) used for
    /// calibration. Distinct wording and roughly 10x the length of that
    /// notice, so this exercises the store's own `Original`/`Header` entries
    /// against real full-file input, independent of the hand-injected
    /// header fallback -- the generality case Task 10's walk-up resolver
    /// depends on.
    const FULL_APACHE2_LICENSE_FILE: &str = r#"                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

   2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

   3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

   5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

   6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

   7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

   8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

   9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

   END OF TERMS AND CONDITIONS

   APPENDIX: How to apply the Apache License to your work.

      To apply the Apache License to your work, attach the following
      boilerplate notice, with the fields enclosed by brackets "[]"
      replaced with your own identifying information. (Don't include
      the brackets!)  The text should be enclosed in the appropriate
      comment syntax for the file format. We also recommend that a
      file or class name and description of purpose be included on the
      same "printed page" as the copyright notice for easier
      identification within third-party archives.

   Copyright 2024 Example Holder

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
"#;

    #[test]
    fn full_realistic_apache2_license_file_detects() {
        assert_eq!(
            global()
                .unwrap()
                .detect(FULL_APACHE2_LICENSE_FILE)
                .as_deref(),
            Some("Apache-2.0")
        );
    }

    /// A complete, realistic BSD-3-Clause `LICENSE` file: a title line, a
    /// realistic copyright line, "All rights reserved.", the three numbered
    /// clauses, and the *full* disclaimer paragraph ("THIS SOFTWARE IS
    /// PROVIDED ... DAMAGE."). Distinct from `BSD3_HEADER` /
    /// `header_texts/bsd-3-clause.txt` above, both of which stop after the
    /// three clauses and omit the disclaimer entirely -- this is the genuine
    /// full-file text, exercising the store's own `Original`/`Header`
    /// entries unassisted, the generality case Task 10's walk-up resolver
    /// depends on.
    const FULL_BSD3_LICENSE_FILE: &str = r#"BSD 3-Clause License

Copyright (c) 2024, Example Corporation
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.
"#;

    #[test]
    fn full_realistic_bsd3_license_file_detects() {
        assert_eq!(
            global().unwrap().detect(FULL_BSD3_LICENSE_FILE).as_deref(),
            Some("BSD-3-Clause")
        );
    }
}
