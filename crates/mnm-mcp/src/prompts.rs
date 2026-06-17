//! MCP prompts surface: the `add_advanced_search_skill` bootstrap prompt that
//! tells the agent to install the advanced-search skill and relay the reload
//! step. See <https://modelcontextprotocol.io/specification/2025-06-18/server/prompts>.

use crate::protocol::{
    ContentBlock, ErrorCode, PromptArgument, PromptDescription, PromptGetParams, PromptGetResult,
    PromptMessage, PromptsListResult, RequestId, Response,
};

/// The one prompt we expose.
pub const ADD_SKILL_PROMPT: &str = "add_advanced_search_skill";

/// Build the `prompts/list` payload.
#[must_use]
pub fn list() -> PromptsListResult {
    PromptsListResult {
        prompts: vec![PromptDescription {
            name: ADD_SKILL_PROMPT,
            description:
                "Install the midnight-advanced-search skill into this session's AI harness so the assistant uses the advanced retrieval playbook automatically. Checks whether it's already present and installs it if not, then tells you how to reload.",
            arguments: vec![
                PromptArgument {
                    name: "harness",
                    description:
                        "Optional comma-separated harnesses (claude-code, codex, opencode, cursor). Omit to auto-detect.",
                    required: false,
                },
                PromptArgument {
                    name: "scope",
                    description: "Optional install scope: user (default) or project.",
                    required: false,
                },
            ],
        }],
    }
}

/// Render `prompts/get`. Validates the optional `harness`/`scope` arguments the
/// same way the CLI / tool do, and embeds the resolved values into the
/// instruction so the agent calls `install_search_skill` with exactly what the
/// user asked for.
#[must_use]
pub fn get(id: RequestId, params: &PromptGetParams) -> Response {
    use mnm_skills::{Harness, Scope};
    use std::str::FromStr as _;

    if params.name != ADD_SKILL_PROMPT {
        return Response::err(
            id,
            ErrorCode::PromptNotFound,
            format!("unknown prompt: {}", params.name),
        );
    }

    // Arguments arrive as a JSON object of string values (MCP prompt args).
    let arg = |key: &str| -> Option<String> {
        params
            .arguments
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };

    // Validate scope if present.
    let scope_arg = arg("scope");
    if let Some(s) = &scope_arg {
        if Scope::from_str(s).is_err() {
            return Response::err(id, ErrorCode::InvalidParams, format!("unknown scope `{s}`"));
        }
    }

    // Validate and split harness list if present — done once, result reused in build_tool_args.
    let harness_arg = arg("harness");
    let harness_tokens: Option<Vec<&str>> = match harness_arg.as_deref() {
        None => None,
        Some(raw) => {
            let mut tokens = Vec::new();
            for tok in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if Harness::from_str(tok).is_err() {
                    return Response::err(
                        id,
                        ErrorCode::InvalidParams,
                        format!("unknown harness `{tok}`"),
                    );
                }
                tokens.push(tok);
            }
            Some(tokens)
        }
    };

    let tool_args = build_tool_args(harness_tokens.as_deref(), scope_arg.as_deref());
    let text = instruction(&tool_args);

    let result = PromptGetResult {
        description: "Install the midnight-advanced-search skill and tell the user how to reload."
            .to_owned(),
        messages: vec![PromptMessage {
            role: "user",
            content: ContentBlock::Text { text },
        }],
    };
    Response::success(id, serde_json::to_value(result).expect("serialize PromptGetResult"))
}

/// Build the JSON the agent should pass to `install_search_skill`, embedding
/// only the supplied arguments (so omitted ones fall through to auto-detect /
/// default).
fn build_tool_args(harness: Option<&[&str]>, scope: Option<&str>) -> String {
    let mut obj = serde_json::Map::new();
    if let Some(tokens) = harness {
        if !tokens.is_empty() {
            obj.insert("harness".to_owned(), serde_json::json!(tokens));
        }
    }
    if let Some(s) = scope {
        obj.insert("scope".to_owned(), serde_json::json!(s));
    }
    serde_json::Value::Object(obj).to_string()
}

/// Render the five-step user instruction embedding the resolved `tool_args` JSON.
fn instruction(tool_args: &str) -> String {
    format!(
        "The user wants the Midnight advanced-search skill installed.\n\n\
         1. Call the `install_search_skill` tool with arguments: {tool_args}\n\
         (An empty object means auto-detect the installed harnesses at user scope.)\n\
         2. The tool is idempotent and returns, per harness, an `action` of \
         `created`, `updated`, or `unchanged`, plus a `reload_step`.\n\
         3. For every harness whose action is `created` or `updated`, tell the user the exact \
         `reload_step` from the tool's response.\n\
         4. If every harness was `unchanged`, tell the user the skill is already installed and \
         current — no reload needed.\n\
         5. Briefly confirm which harnesses and paths were written."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RequestId;

    fn params(name: &str, args: serde_json::Value) -> PromptGetParams {
        PromptGetParams {
            name: name.to_owned(),
            arguments: args,
        }
    }

    #[test]
    fn list_declares_optional_args() {
        let l = list();
        assert_eq!(l.prompts.len(), 1);
        assert_eq!(l.prompts[0].name, ADD_SKILL_PROMPT);
        let names: Vec<_> = l.prompts[0].arguments.iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["harness", "scope"]);
        assert!(l.prompts[0].arguments.iter().all(|a| !a.required));
    }

    #[test]
    fn get_unknown_prompt_errors() {
        let r = get(RequestId::Number(1), &params("nope", serde_json::json!({})));
        assert!(r.error.is_some());
    }

    #[test]
    fn get_no_args_embeds_empty_object() {
        let r = get(RequestId::Number(1), &params(ADD_SKILL_PROMPT, serde_json::json!({})));
        let v = serde_json::to_value(&r).unwrap();
        let text = v["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("arguments: {}"));
        assert_eq!(v["result"]["messages"][0]["role"], "user");
    }

    #[test]
    fn get_embeds_supplied_args() {
        let r = get(
            RequestId::Number(1),
            &params(
                ADD_SKILL_PROMPT,
                serde_json::json!({ "harness": "cursor,codex", "scope": "project" }),
            ),
        );
        let v = serde_json::to_value(&r).unwrap();
        let text = v["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("\"harness\":[\"cursor\",\"codex\"]"));
        assert!(text.contains("\"scope\":\"project\""));
    }

    #[test]
    fn get_rejects_bad_scope_and_harness() {
        let bad_scope = get(
            RequestId::Number(1),
            &params(ADD_SKILL_PROMPT, serde_json::json!({ "scope": "global" })),
        );
        assert!(bad_scope.error.is_some());
        let bad_h = get(
            RequestId::Number(1),
            &params(ADD_SKILL_PROMPT, serde_json::json!({ "harness": "windsurf" })),
        );
        assert!(bad_h.error.is_some());
    }
}
