//! The public contract artifact must mirror `tools::list()` exactly.
//! `tests/contract/mcp-tools.json` keeps a prose preamble (`$schema`,
//! `title`, `description`, `version`, `error_envelope`); its `tools` key is
//! the wire-format `tools/list` array and its `errorSchema` key is the
//! machine-readable error contract — both generated, never hand-edited.
//! Regenerate with:
//!
//! ```sh
//! VOYAGE_API_KEY= REGENERATE_CONTRACT=1 cargo test -p mnm-mcp --test contract_sync
//! ```

/// Path to the contract artifact, relative to this crate's manifest.
const CONTRACT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract/mcp-tools.json");

#[test]
fn mcp_tools_json_mirrors_tools_list() {
    let manifest = serde_json::to_value(mnm_mcp::tools::list()).expect("serialize tools::list()");
    let tools = manifest.get("tools").expect("manifest has a tools array");
    let error_schema = mnm_mcp::schemas::error_output_schema();

    if std::env::var("REGENERATE_CONTRACT").is_ok() {
        // Preserve the prose preamble; regenerate the `tools` and `errorSchema`
        // keys from the single source of truth in code.
        let mut doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(CONTRACT_PATH).expect("read mcp-tools.json"),
        )
        .expect("parse mcp-tools.json");
        let obj = doc
            .as_object_mut()
            .expect("mcp-tools.json is a JSON object");
        obj.insert("tools".to_owned(), tools.clone());
        obj.insert("errorSchema".to_owned(), error_schema);
        let pretty = serde_json::to_string_pretty(&doc).expect("pretty-print contract");
        std::fs::write(CONTRACT_PATH, format!("{pretty}\n")).expect("write mcp-tools.json");
        return;
    }

    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(CONTRACT_PATH).expect("read mcp-tools.json"))
            .expect("parse mcp-tools.json");

    assert_eq!(
        on_disk.get("tools"),
        Some(tools),
        "contracts/mcp-tools.json `tools` is stale — regenerate with \
         REGENERATE_CONTRACT=1 cargo test -p mnm-mcp --test contract_sync"
    );

    assert_eq!(
        on_disk.get("errorSchema"),
        Some(&error_schema),
        "contracts/mcp-tools.json `errorSchema` is stale — regenerate with \
         REGENERATE_CONTRACT=1 cargo test -p mnm-mcp --test contract_sync"
    );

    // The preamble prose states the tool count; keep it honest too.
    let count = tools.as_array().expect("tools is an array").len();
    let description = on_disk
        .get("description")
        .and_then(serde_json::Value::as_str)
        .expect("preamble has a description");
    assert!(
        description.contains(&format!("{count} tools")),
        "preamble description must state the current tool count ({count} tools)"
    );
}

/// The closed error-code set must agree across all three places it appears:
/// the code (`ErrorKind`), the discoverable `errorSchema` enum, and the prose
/// `error_envelope.codes` map. This is what makes the error contract honest.
#[test]
fn error_codes_agree_across_code_schema_and_prose() {
    use std::collections::BTreeSet;

    let from_code: BTreeSet<String> = mnm_mcp::render::ErrorKind::ALL
        .iter()
        .map(|k| k.code().to_owned())
        .collect();

    let schema = mnm_mcp::schemas::error_output_schema();
    let from_schema: BTreeSet<String> = schema["properties"]["error"]["properties"]["code"]["enum"]
        .as_array()
        .expect("errorSchema code.enum is an array")
        .iter()
        .map(|v| v.as_str().expect("enum entry is a string").to_owned())
        .collect();

    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(CONTRACT_PATH).expect("read mcp-tools.json"))
            .expect("parse mcp-tools.json");
    let from_prose: BTreeSet<String> = on_disk["error_envelope"]["codes"]
        .as_object()
        .expect("error_envelope.codes is an object")
        .keys()
        .cloned()
        .collect();

    assert_eq!(from_schema, from_code, "errorSchema enum must mirror ErrorKind::ALL");
    assert_eq!(
        from_prose, from_code,
        "error_envelope.codes prose must enumerate exactly the ErrorKind code set"
    );
}
