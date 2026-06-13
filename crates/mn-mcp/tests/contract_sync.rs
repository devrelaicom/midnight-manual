//! The public contract artifact must mirror `tools::list()` exactly.
//! `tests/contract/mcp-tools.json` keeps a prose preamble (`$schema`,
//! `title`, `description`, `version`, `error_envelope`); its `tools` key is
//! the wire-format `tools/list` array and is generated, never hand-edited.
//! Regenerate with:
//!
//! ```sh
//! VOYAGE_API_KEY= REGENERATE_CONTRACT=1 cargo test -p mn-mcp --test contract_sync
//! ```

/// Path to the contract artifact, relative to this crate's manifest.
const CONTRACT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract/mcp-tools.json");

#[test]
fn mcp_tools_json_mirrors_tools_list() {
    let manifest = serde_json::to_value(mn_mcp::tools::list()).expect("serialize tools::list()");
    let tools = manifest.get("tools").expect("manifest has a tools array");

    if std::env::var("REGENERATE_CONTRACT").is_ok() {
        // Preserve the prose preamble; regenerate only the `tools` key.
        let mut doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(CONTRACT_PATH).expect("read mcp-tools.json"),
        )
        .expect("parse mcp-tools.json");
        doc.as_object_mut()
            .expect("mcp-tools.json is a JSON object")
            .insert("tools".to_owned(), tools.clone());
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
         REGENERATE_CONTRACT=1 cargo test -p mn-mcp --test contract_sync"
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
