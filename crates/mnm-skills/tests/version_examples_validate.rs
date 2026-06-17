//! Every `version_satisfies` example value shipped in the skill must be
//! accepted by the real request parser (the guard the 2026-06 range-syntax
//! doc bug lacked).

use std::path::Path;

#[test]
fn skill_version_satisfies_examples_parse() {
    // Root at `assets/` and recurse: the bundle lives in
    // `assets/midnight-advanced-search/`, and rooting at `assets/` keeps the
    // guard correct if the bundle layout moves.
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut checked = 0usize;
    let re = regex::Regex::new(r#""version_satisfies"\s*:\s*"([^"]+)""#).unwrap();
    for entry in walkdir::WalkDir::new(&assets)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let body = std::fs::read_to_string(entry.path()).unwrap();
        for cap in re.captures_iter(&body) {
            let value = &cap[1];
            assert!(
                mnm_core::version_match::parse_request(value).is_some(),
                "{}: `{value}` is not a valid version/range",
                entry.path().display()
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 4,
        "expected version_satisfies examples in the skill (found {checked})"
    );
}
