# Advanced search skills — design

**Date:** 2026-05-29
**Status:** draft
**Touches:** new lib crate `mn-skills` (embedded `SKILL.md`, harness registry,
detection, idempotent install/remove/status); `mn-cli` (new `skills` noun:
`add`/`status`/`remove`); `mn-mcp` (new `install_search_skill` tool + net-new
`prompts` capability with `prompts/list`/`prompts/get` and the
`add_advanced_search_skill` prompt); `mn-telemetry` (new `CliCommandName::Skills`
and `McpToolName::InstallSearchSkill` arms); workspace `Cargo.toml` (new member);
docs (`README.md` Cursor row, `specs/001-rag-platform/contracts/mcp-tools.json`,
new `mcp-prompts.json`); the canonical `SKILL.md` asset.

This builds the **Advanced Search Skill** feature promised by the README's
"Advanced search skills" section (PR #63, branch `docs/promo-readme`). That
section is the UX/product source of truth; this design matches its command
names, flags, tool name, and prompt name, and updates the one place the README
has drifted from the live ecosystem (Cursor — see Non-goals / §"Cursor").

## Problem

The MCP server gives an agent powerful retrieval primitives (hybrid search,
RRF multi-query, cross-encoder rerank, trust scoring, chunk/document
navigation), but nothing teaches the agent *how to combine them*. Left to its
own devices an agent fires one naive query and stops. The fix is a persistent,
auto-loaded **Agent Skill** (`SKILL.md`) — a playbook the harness loads on its
own — plus a friction-free way to install that skill into whichever AI harness
the user runs (CLI verb, MCP tool, and an MCP prompt that lets the agent
install it for the user on request).

## Goals

- **One canonical `SKILL.md`** in the repo, embedded into the binaries via
  `include_str!`, kept DRY with `docs/cookbook/query-enhancement.md` and the
  `search` tool description. Folder/skill name: `midnight-advanced-search`.
- **`mnm skills add`** — installs the skill into the user's detected harness(es),
  idempotently, printing where it wrote and the exact reload step per harness.
  Plus `mnm skills status` and `mnm skills remove`.
- **MCP tool `install_search_skill`** — an agent-triggerable install that writes
  directly and returns status + harnesses + exact paths + per-harness reload
  step.
- **MCP prompt `add_advanced_search_skill`** (`/mnm:add-advanced-search-skill`)
  — instructs the agent to call `install_search_skill` and relay the reload
  step. Requires standing up a net-new `prompts` capability on `mn-mcp`.
- **Shared install logic** lives in one crate (`mn-skills`) consumed by both
  `mn-cli` and `mn-mcp`. No duplicated path/detection/write logic.
- **Safety:** writes only into an owned `midnight-advanced-search/` directory;
  re-running `add` updates in place; filesystem-only and unit-testable with a
  fake home/cwd.

## Non-goals (out of scope)

- **No `.mdc` Cursor adapter.** Cursor 2.4 (2026-01-22) supports `SKILL.md`
  Agent Skills natively, including file-based user-global paths — so all four
  harnesses are "write the same `SKILL.md` to the right dir." The README's
  Cursor row (which still describes a `.mdc` Project Rule) is updated in this
  PR to match.
- **No `list` verb.** Exactly one skill ships; `status` already reports it
  per harness. (Revisit if the product ships multiple skills.)
- **No Codex `agents/openai.yaml` sidecar.** Optional Codex-specific UI
  metadata; not needed for a portable skill.
- **No cross-read deduplication.** OpenCode and Cursor cross-read each other's /
  Claude's / Codex's skill dirs, so a single write *could* cover several
  harnesses. We deliberately do **not** exploit this — each detected harness
  gets its own copy in its own native dir (predictable, self-contained,
  survives uninstalling a neighbour). See "Write strategy."
- **No "coming soon" harnesses** (Gemini CLI, Windsurf, Zed, Cline, Continue).
- **No editing of user-modified copies beyond the owned dir.** We own
  `midnight-advanced-search/`; we never touch anything else.
- **No standalone MCP `check`/status tool.** The README's "checks whether
  already present, installs if not" is satisfied by the idempotent install
  reporting a per-harness `unchanged`/`created`/`updated` action. CLI-side
  inspection is `mnm skills status`.

## Verified harness matrix (re-verified against live docs, 2026-05-29)

Source-checked deltas from the original brief's matrix are folded in here.
`<name>` is always `midnight-advanced-search`.

| Harness | Format | User target | Project target |
|---|---|---|---|
| **claude-code** | `SKILL.md` (Agent Skill) | `~/.claude/skills/<name>/SKILL.md` | `.claude/skills/<name>/SKILL.md` |
| **codex** | `SKILL.md` (open standard) | `~/.agents/skills/<name>/SKILL.md` | `<repo-root>/.agents/skills/<name>/SKILL.md` |
| **opencode** | `SKILL.md` (native) | `~/.config/opencode/skills/<name>/SKILL.md` | `.opencode/skills/<name>/SKILL.md` |
| **cursor** | `SKILL.md` (Agent Skill, Cursor 2.4+) | `~/.cursor/skills/<name>/SKILL.md` | `.cursor/skills/<name>/SKILL.md` |

**Frontmatter (open Agent Skills standard).** `name` (1–64 chars, regex
`^[a-z0-9]+(-[a-z0-9]+)*$`, **must equal the folder name**) and `description`
(1–1024 chars). Claude Code treats both as *recommended* rather than strictly
required and supports a longer description budget, but writing both with a
≤1024-char description is the portable, correct choice. Extra frontmatter keys
(`metadata`) are tolerated by harnesses that don't consume them.

**Reload mechanics (uniform story).** All four auto-discover skills; a session
restart is the reliable fallback. Specifics folded into the per-harness reload
string:
- claude-code: live-watched within a session **if** the top-level `skills/`
  dir already existed at session start; if `add` *creates* that top-level dir
  for the first time, restart is required. No reload command.
- codex: auto-detected; restart if it doesn't appear. (`/skills` or `$` to
  invoke/list.)
- opencode: restart the session (no documented hot-reload).
- cursor: auto-discovered on startup; restart to be safe.

**Cross-read overlap (informational, not exploited).** OpenCode reads
`~/.claude/skills`, `~/.agents/skills`, `.claude/skills`, `.agents/skills`
(both scopes). Cursor 2.4 reads `~/.claude/skills`, `~/.agents/skills`,
`.claude/skills`, `.codex/skills`. Codex does NOT merge same-named skills across
scopes (both appear in the selector). Accepted consequence of "one copy per
harness": a user with both Codex and OpenCode gets `midnight-advanced-search`
in two dirs OpenCode reads; since harnesses key skills by `name` and the content
is identical, the worst case is a harmless duplicate entry. Documented, not
mitigated.

## Architecture

### Crate `mn-skills`

New workspace member `crates/mn-skills` (lib only). Public surface:

- `pub const SKILL_NAME: &str = "midnight-advanced-search";`
- The embedded skill body: `include_str!("../assets/midnight-advanced-search/SKILL.md")`,
  exposed as `pub fn skill_markdown() -> &'static str`.
- `pub enum Harness { ClaudeCode, Codex, OpenCode, Cursor }` with id strings
  `claude-code` / `codex` / `opencode` / `cursor` (parse + display).
- `pub enum Scope { User, Project }` (default `User`).
- A **harness registry**: for each `Harness`, the user/project target dir, the
  detection markers (user + project), and the templated reload step. Pure data
  + small helpers; no globals.
- **Path resolution** takes an injected environment (home dir + cwd + a
  repo-root finder) so tests use a fake `$HOME`/CWD. Real callers pass a
  `StdEnv`-style impl. No direct `std::env::var("HOME")` deep in the logic.
- **Detection:** a harness is "detected" at a scope when its marker for that
  scope exists. User markers: `~/.claude/`, `~/.codex/`, `~/.config/opencode/`,
  `~/.cursor/`. Project markers (searched from cwd up to repo root): `.claude/`,
  (`.codex/` or `AGENTS.md`), `.opencode/`, `.cursor/`. Repo root = nearest
  ancestor containing `.git/` (fallback: cwd).
- **Install:** `install(targets, scope, env) -> InstallReport`. For each target
  harness, resolve the owned dir, write `SKILL.md` idempotently, and record a
  per-harness outcome. Creates parent dirs as needed.
- **Remove:** `remove(targets, scope, env) -> RemoveReport`. Deletes only the
  owned `<name>/` dir; reports `removed` | `absent`.
- **Status:** `status(targets_or_all, env) -> StatusReport`. For each harness ×
  scope: detected?, installed?, and `up_to_date` | `stale` (installed content
  hash vs embedded content hash).
- Report types are plain serializable structs (`serde`) so the CLI renders text
  or `--json`, and the MCP tool serializes the same shapes into its response.

```text
crates/mn-skills/
├── Cargo.toml
├── assets/
│   └── midnight-advanced-search/
│       └── SKILL.md          # the one canonical source, include_str!'d
└── src/
    ├── lib.rs                # re-exports + skill_markdown()
    ├── harness.rs            # Harness, Scope, registry, reload templates
    ├── detect.rs             # marker-based detection + repo-root finder
    ├── install.rs            # install / remove / status + report types
    └── env.rs                # Home/Cwd injection trait + StdEnv impl
```

### Install action semantics (idempotency)

Per harness/scope, `add` computes one of:
- `created` — owned dir/file did not exist; written.
- `updated` — existed with different content; overwritten with embedded body.
- `unchanged` — existed with byte-identical content; no write.

`add` is therefore safe to re-run. `status` uses the same comparison to report
`stale` (installed content differs from embedded). `remove` reports `removed`
or `absent`.

### CLI — `mnm skills {add,status,remove}`

New `crates/mn-cli/src/commands/skills/` mirroring `chunks/`:

```text
skills/
├── mod.rs      # SkillsCmd { Add, Status, Remove } + dispatcher
├── add.rs
├── status.rs
└── remove.rs
```

- `mnm skills add [--harness <comma-list>] [--scope user|project]`
- `mnm skills status [--harness <comma-list>] [--scope user|project]`
- `mnm skills remove [--harness <comma-list>] [--scope user|project]`

Flags:
- `--harness` — comma list of `claude-code`/`codex`/`opencode`/`cursor`.
  Omitted → **auto-detect**. Explicit values **override** detection (force).
- `--scope` — `user` (default) or `project`.
- Global `--json` honored (emit the report struct).

Behavior:
- No harness detected and none specified → friendly error, non-zero exit,
  listing the markers probed and how to force with `--harness`.
- Human output: per harness, the absolute path written and the exact reload
  step. `status` renders a small table (harness · scope · detected · installed ·
  up-to-date/stale).

Wiring: `Command::Skills(commands::skills::Args)` in `cli.rs`; new
`CliCommandName::Skills` arm in `mn-telemetry` and `cli_command_name`. Not an
admin command (visible in `--help`).

### MCP tool — `install_search_skill`

- Added to `tools::list()` (11 → 12 tools) with an input schema:
  `{ harness?: string[], scope?: "user"|"project" (default "user") }`,
  `additionalProperties:false`.
- New arm in `dispatch_tool_inner` that calls `mn_skills::install(...)` (auto-
  detect when `harness` absent; explicit override when present) and returns a
  single text content block whose body is JSON:
  `{ status, scope, harnesses: [{ harness, scope, path, action, reload_step }],
  not_detected: [...], skill_name }`.
- New `McpToolName::InstallSearchSkill` arm so the existing
  `every_manifest_tool_has_a_telemetry_name` test stays green.
- The MCP server already runs as the local user and writes the model cache;
  writing the skill dir is consistent with that trust surface. The response
  fully discloses every path written.

### MCP prompts capability (net-new) + `add_advanced_search_skill`

`mn-mcp` currently advertises only `tools`. Add `prompts`:

- `protocol.rs`: add `PromptsCapability { list_changed: bool }`, extend
  `ServerCapabilities` with `prompts: PromptsCapability` (`listChanged:false`).
  Add `PromptDescription` (`name`, `description`, `arguments: [PromptArgument]`),
  `PromptsListResult`, `PromptGetParams` (`name`, `arguments?`),
  `PromptMessage { role, content: ContentBlock }`, `PromptGetResult
  { description, messages }`.
- `server.rs`: handle `prompts/list` and `prompts/get` alongside the existing
  methods. Unknown prompt name → `InvalidParams`/`MethodNotFound`-style error.
- A small `prompts.rs` (or fold into `tools.rs`) producing the prompt manifest
  and the rendered messages.

Prompt definition:
- name: `add_advanced_search_skill`
- description: tuned so a client lists it as the "install the advanced search
  skill" action; surfaced to users as `/mnm:add-advanced-search-skill` (clients
  may namespace, e.g. `/mcp__midnight-manual__add_advanced_search_skill`).
- arguments (both **optional**, mirroring the CLI flags — MCP prompt arguments
  are strings):
  - `harness` — comma-separated list of `claude-code`/`codex`/`opencode`/
    `cursor`. Omitted → auto-detect.
  - `scope` — `user` (default) or `project`.
  Declared in `prompts/list` with `required: false`. `prompts/get` validates
  them the same way the CLI does (unknown harness id / bad scope →
  `InvalidParams`).
- `prompts/get` returns one `user`-role message instructing the agent to:
  1. call the `install_search_skill` tool — forwarding `harness`/`scope` when
     they were supplied to the prompt, otherwise letting the tool auto-detect.
     The message text embeds the resolved argument values so the agent calls the
     tool with exactly what the user asked for. The tool is idempotent and
     reports, per harness, whether it was `created`/`updated`/`unchanged`;
  2. for harnesses where the action was `created` or `updated`, tell the user
     the exact per-harness reload step from the tool's response;
  3. if every harness was `unchanged`, tell the user the skill is already
     installed and current.

The prompt's `harness`/`scope` pass straight through to `install_search_skill`,
which already accepts the same two arguments — so the prompt, the MCP tool, and
`mnm skills add` all share one argument vocabulary.

### The canonical `SKILL.md`

- Lives at `crates/mn-skills/assets/midnight-advanced-search/SKILL.md`;
  embedded via `include_str!`. Single source of truth for the skill body that
  the CLI and MCP tool both write.
- Frontmatter: `name: midnight-advanced-search`; `description:` ≤1024 chars,
  written to **auto-trigger** when the agent is researching/searching the
  Midnight corpus (mention Midnight, Compact, the corpus, retrieval); plus
  `metadata: { source: midnight-manual }` (version/stamp optional — stale
  detection uses content hash, so a version field is not required).
- Body — the playbook. For **each** technique: *when to use it* + *how to do it
  with the real tools*:
  - **HyDE** — draft a 1–2 sentence hypothetical answer; send it as a second
    entry in `queries[]` alongside the question.
  - **Multi-query** — 2–3 paraphrases varying vocabulary/breadth in one
    `queries[]` call (RRF-fused, k=60).
  - **Step-back** — pair the specific question with a more abstract framing.
  - **Lexical anchoring** — put exact identifiers / error codes verbatim into a
    query so the FTS half of hybrid search nails the exact match.
  - **Symbol-aware code search** — scope with `filters.package` /
    `filters.language_target`, then navigate hits by their `symbol_path` and
    with `get_chunk_parents` / `get_chunk_next`.
  - **Retrieve-read-retrieve** — broad search → read with
    `get_chunk_next`/`get_chunk_parents`/`get_document*` → refine with
    newly-learned terms → search again.
  - **Trust-weighted selection** — rank/prune on each result's `trust_score`
    and `confidence_factors` (attribution, verification, freshness,
    version-match); prefer authoritative, version-matched sources.
  - **Cross-source comparison** — pull from multiple sources and surface
    disagreement; the server has **no** contradiction detection, so the agent
    must compensate.
  - A recommended **default loop** (e.g. list_sources/scope → multi-query or
    HyDE → trust-weighted read → navigate → refine).
  - A **D25 cost note**: multi-query costs `max(1, distinct queries)` tokens —
    don't fan out wastefully.
- **DRY:** the SKILL.md is the decision playbook and **links to**
  `docs/cookbook/query-enhancement.md` for the long-form worked examples
  (HyDE/multi-query/step-back) rather than duplicating their JSON. The `search`
  tool description keeps its terse inline hints + cookbook pointer. A unit test
  asserts the embedded body parses: valid YAML frontmatter, `name` ==
  `midnight-advanced-search` == folder name, `description` non-empty and
  ≤1024 chars.

## Docs / contract / version updates

- `specs/001-rag-platform/contracts/mcp-tools.json`: add the
  `install_search_skill` tool entry (input/output schema, description); bump the
  contract `version` `1.1.0 → 1.2.0`; update the count note (11 → 12 tools).
- New `specs/001-rag-platform/contracts/mcp-prompts.json`: document the
  `prompts` capability and the `add_advanced_search_skill` prompt (name,
  description, arguments, returned message shape).
- `README.md` "Advanced search skills" section: update the **Cursor row** of
  the "Supported harnesses" table to `SKILL.md (Agent Skill, Cursor 2.4+)` with
  `~/.cursor/skills/` · `.cursor/skills/`, and adjust the "adapted rule where it
  isn't" sentence since all four are now `SKILL.md`. Everything else
  (`/mnm:add-advanced-search-skill`, `mnm skills add`, `--harness`/`--scope`,
  `install_search_skill`, technique table) already matches.
- Treat the surface change as a **MINOR additive** semver bump. release-please
  drives crate `Cargo.toml` versions; this work does not hand-edit them.

## Testing

Every task ends green on:
`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.

- **`mn-skills`** (no Docker, no network; `tempfile` + injected home/cwd):
  detection per harness × scope; path resolution incl. Codex repo-root walk;
  scope mapping; install `created`/`updated`/`unchanged`; remove
  `removed`/`absent`; status `up_to_date`/`stale`/`not installed`; explicit
  `--harness` overriding detection; "no harness detected" error path; embedded
  `SKILL.md` frontmatter validity (name==folder, description ≤1024).
- **`mn-mcp`** (over the real JSON-RPC framing): `initialize` advertises the
  `prompts` capability; `prompts/list` lists `add_advanced_search_skill` with
  its optional `harness`/`scope` arguments; `prompts/get` returns the expected
  message shape both with no args (auto-detect) and with `harness`/`scope`
  supplied (the resolved values appear in the message), errors on an unknown
  prompt name, and rejects a bad `scope` / unknown `harness` as `InvalidParams`;
  `install_search_skill` dispatch writes into a fake home and returns the
  documented JSON; telemetry-coverage test still passes with the new tool name.
- **`mn-cli`**: arg parsing for `add`/`status`/`remove` (`--harness`/`--scope`);
  render of text + `--json` reports; error on no-harness-detected.

## Implementation order (high level — detailed plan follows in writing-plans)

1. `mn-skills` crate skeleton + workspace member + embedded `SKILL.md` (stub
   body + frontmatter validity test).
2. Harness registry + detection + path resolution (+ tests).
3. Install / remove / status + report types (+ tests).
4. Author the real `SKILL.md` playbook content; wire DRY links to the cookbook.
5. `mnm skills` CLI noun (add/status/remove) + telemetry arm (+ tests).
6. `mn-mcp` `install_search_skill` tool + telemetry arm + contract update
   (+ tests).
7. `mn-mcp` `prompts` capability + `add_advanced_search_skill` + prompts
   contract (+ tests).
8. README Cursor-row update + final cross-doc consistency pass.
