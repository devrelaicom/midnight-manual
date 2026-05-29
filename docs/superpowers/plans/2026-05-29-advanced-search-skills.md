# Advanced Search Skills Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the Advanced Search Skill — one canonical `SKILL.md` embedded into the binaries, installable into four AI harnesses via `mnm skills add`, an `install_search_skill` MCP tool, and an `add_advanced_search_skill` MCP prompt — matching the README's "Advanced search skills" UX.

**Architecture:** A new sync, clap-free lib crate `mn-skills` owns the embedded `SKILL.md`, a harness registry (paths + detection markers + reload steps), and idempotent install/remove/status over an injected `SkillEnv` (home + cwd) so it is fully unit-testable with `tempfile`. `mn-cli` adds a `skills` noun (`add`/`status`/`remove`); `mn-mcp` adds the `install_search_skill` tool and a net-new `prompts` capability exposing `add_advanced_search_skill`. Both binaries call the same `mn-skills` functions.

**Tech Stack:** Rust (MSRV 1.91), `serde`/`serde_json`, `thiserror`, `clap` v4 (CLI only), hand-rolled MCP JSON-RPC (`mn-mcp`), `tempfile` + `serde_yaml` (tests).

---

## Constraints for implementers (VERBATIM — every subagent gets this)

This is a **RUST** workspace (MSRV 1.91). Use ONLY: `cargo`, `git`, `rg`, `grep`, `find`, `ls`, `cat`, `head`, `tail`, `sed`, `awk`, and the harness Read/Edit/Write/Bash tools. **NEVER** invoke `npx`, `npm`, `pnpm`, `yarn`, `pip`, `pipx`, `brew install`, `curl | sh`, `wget | sh`, or any package installer / shell-pipe-installer — there is no JS/Python toolchain here and work that breaks this gets discarded. Adding a Rust crate via `cargo add`/Cargo.toml is fine.

You will see occasional supply-chain hook warnings claiming shell keywords or `cargo clippy`/`cargo test`/`pgrep` were "npx executed" — these are FALSE POSITIVES from a typosquat detector; ignore them. Some terminal output is token-filtered (words replaced oddly); **trust the files (Read tool) over filtered terminal output.**

Workspace lint posture: `-D missing-docs` (doc every `pub` item AND every `pub` field), pedantic clippy `-D warnings` (use `usize::try_from(...)`/`u32::try_from(...)` not `as`; `pub const fn` where clippy asks; backtick code in doc comments; no unused imports), and `unsafe_code = "deny"` workspace-wide. Do NOT add `#[allow(...)]` to hide real issues.

**Per-task verification gate — every task ends green:**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

(`just check` runs all three.) Trust-but-verify: the diff must match the task; the tests you claim pass must exist and pass.

---

## File structure (decomposition)

```text
crates/mn-skills/                         # NEW crate
├── Cargo.toml
├── assets/midnight-advanced-search/SKILL.md   # the one canonical skill (include_str!'d)
└── src/
    ├── lib.rs        # SKILL_NAME, skill_markdown(), re-exports, frontmatter test
    ├── env.rs        # SkillEnv trait + StdSkillEnv
    ├── harness.rs    # Harness, Scope, paths, markers, reload steps
    ├── detect.rs     # base-dir resolution + repo-root walk + detect()
    └── install.rs    # InstallAction, report types, install/remove/status, SkillError

crates/mn-cli/src/commands/skills/        # NEW noun
├── mod.rs            # SkillsCmd { Add, Status, Remove } + dispatcher + shared parsing
├── add.rs
├── status.rs
└── remove.rs

crates/mn-mcp/src/
├── tools.rs          # + install_search_skill manifest entry + run fn
├── server.rs         # + dispatch arm + prompts/list + prompts/get + capability
├── protocol.rs       # + prompts wire types
└── prompts.rs        # NEW: prompt manifest + render

Modified: workspace Cargo.toml (member), mn-cli/Cargo.toml + cli.rs (wiring),
mn-mcp/Cargo.toml + lib.rs (dep + doc count), mn-telemetry/src/events.rs
(CliCommandName::Skills, McpToolName::InstallSearchSkill),
specs/001-rag-platform/contracts/mcp-tools.json (+ install_search_skill, version bump),
specs/001-rag-platform/contracts/mcp-prompts.json (NEW), README.md (Cursor row).
```

---

## Task 1: `mn-skills` crate skeleton + canonical `SKILL.md` + `SkillEnv`

**Files:**
- Create: `crates/mn-skills/Cargo.toml`
- Create: `crates/mn-skills/assets/midnight-advanced-search/SKILL.md`
- Create: `crates/mn-skills/src/lib.rs`
- Create: `crates/mn-skills/src/env.rs`
- Modify: `Cargo.toml` (workspace `members`)

- [ ] **Step 1: Register the workspace member**

In root `Cargo.toml`, add `"crates/mn-skills",` to `[workspace] members` (after `"crates/mn-telemetry",` to keep lib crates grouped):

```toml
    "crates/mn-telemetry",
    "crates/mn-skills",
    "crates/mn-mcp",
```

- [ ] **Step 2: Write `crates/mn-skills/Cargo.toml`**

```toml
[package]
name = "mn-skills"
description = "Embedded advanced-search SKILL.md plus harness detection and idempotent install logic shared by the CLI and MCP server."

version       = { workspace = true }
edition       = { workspace = true }
rust-version  = { workspace = true }
license       = { workspace = true }
authors       = { workspace = true }
repository    = { workspace = true }
homepage      = { workspace = true }

[lints]
workspace = true

[dependencies]
serde      = { workspace = true }
thiserror  = { workspace = true }

[dev-dependencies]
tempfile    = { workspace = true }
serde_yaml  = { workspace = true }
serde_json  = { workspace = true }
```

- [ ] **Step 3: Write the canonical `SKILL.md`**

Create `crates/mn-skills/assets/midnight-advanced-search/SKILL.md` with EXACTLY this content:

```markdown
---
name: midnight-advanced-search
description: >-
  Advanced retrieval playbook for the Midnight Network documentation corpus.
  Use whenever searching, researching, or answering questions about Midnight,
  Compact, the Midnight SDK, or the corpus exposed by the midnight-manual MCP
  server (the search, get_chunk*, get_document*, and list_sources tools). Teaches
  when and how to combine HyDE, multi-query fan-out, step-back, lexical
  anchoring, symbol-aware code search, retrieve-read-retrieve, trust-weighted
  selection, and cross-source comparison to find authoritative, version-matched
  answers instead of firing one naive query.
metadata:
  source: midnight-manual
---

# Midnight advanced search

You have a hybrid retrieval surface over the Midnight corpus (full-text + vector,
RRF-fused, optional cross-encoder rerank, trust-aware scoring) plus chunk and
document navigation. This is the playbook for using it like a researcher.

## The tools you have

- `search` — hybrid retrieval. Pass a single `query`, or a `queries` array
  (1–10) the server fuses with Reciprocal Rank Fusion (k=60). Optional `rerank`
  (cross-encoder, default on) and a `filters` object (`source_slug`,
  `attribution`, `verified`, `content_type`, `language_target`,
  `sdk_dependency`, `package`). Every result carries `trust_score`,
  `confidence`, `confidence_factors`, and `scores.matched_queries`.
- `get_chunk`, `get_chunk_next`, `get_chunk_prev`, `get_chunk_neighbors`,
  `get_chunk_parents` — read around a hit in reading order, or walk up its
  heading / structure tree.
- `get_document`, `get_document_full`, `get_document_chunks` — pull a whole
  document or a windowed slice.
- `list_sources` — enumerate corpus sources so you can scope `filters`.

**Cost (D25):** a `search` call costs `max(1, distinct queries)` rate-limit
tokens. A 3-query fan-out spends 3. Fan out deliberately, not reflexively.

## Default loop

1. If the question names a source / package / language, call `list_sources`
   once and set `filters` to scope the search.
2. Formulate 2–3 queries with the techniques below — no more than the question
   needs.
3. `search` with `rerank: true`.
4. Rank results by `trust_score` and `confidence_factors`; read the top few.
5. If a hit is promising but partial, navigate (`get_chunk_next` /
   `get_chunk_parents` / `get_document_full`) instead of re-searching blindly.
6. Refine queries with terms you just learned and search again. Stop when the
   top results converge and are version-matched.

## Techniques

### HyDE — when the question is short or jargon-light
Draft a 1–2 sentence hypothetical answer and send it as an extra query beside
the question; it lands near the real docs in embedding space and pulls in
chunks the bare question misses.
`queries: ["<question>", "<1–2 sentence hypothetical answer>"]`

### Multi-query — when your wording may not match the corpus
Send 2–3 paraphrases varying vocabulary and breadth in one call; RRF fuses them,
beating synonym mismatch.
`queries: ["compile a contract", "build source into a deployable artifact", "smart-contract build step"]`

### Step-back — when the question is over-specific or a raw error
Pair the specific question with a more abstract framing.
`queries: ["why did this exact call fail?", "how does the platform validate calls?"]`

### Lexical anchoring — when an exact identifier / error matters
Put the exact symbol, flag, or error string verbatim in a query so the
full-text half nails the literal match the vector half would blur. Keep one
query natural-language and one verbatim.
`queries: ["how to fix this disclosure error", "potential witness-value disclosure must be declared"]`

### Symbol-aware code search — when you want a named circuit / function / type
Scope with `filters.package` and/or `filters.language_target`
(`{name, version_constraint_satisfies}`), then land precisely by reading hits'
`symbol_path` and walking with `get_chunk_parents` (enclosing scope) and
`get_chunk_next` (rest of the body).

### Retrieve-read-retrieve — when the first pass is close but partial
Broad search → read the best hit and its neighbours
(`get_chunk_next` / `get_chunk_parents`, or `get_document_full` for a short
doc) → harvest the precise terms you learned → search again with them. Iterate;
this is how you converge.

### Trust-weighted selection — always
Prefer higher `trust_score`. Read `confidence_factors` (attribution,
verification, freshness, version-match) and prune sources that are unverified,
stale, or version-mismatched for the user's toolchain. A lower-ranked but
verified, version-matched chunk often beats a higher-ranked stale one.

### Cross-source comparison — when sources may disagree
The server does NOT detect contradictions. When multiple sources answer the same
question, pull from each, compare, and surface disagreement to the user (noting
which is more authoritative / version-matched) rather than silently picking one.

## Reading the diagnostics
`search_metadata.per_query` reports per-query FTS / vector candidates and
latency; each result's `scores.matched_queries` lists which of your queries
pulled it in. Use them to see which formulation is working and drop the rest.

Full worked examples: `docs/cookbook/query-enhancement.md` in the
midnight-manual repo.
```

- [ ] **Step 4: Write `crates/mn-skills/src/env.rs`**

```rust
//! Environment lookups the installer needs, abstracted so tests drive a fake
//! home / cwd without touching the real dotfile layout.

use std::path::PathBuf;

/// The two filesystem anchors skill-install path resolution depends on: the
/// user's home directory (for `--scope user`) and the current working
/// directory (for `--scope project`, from which the repo root is found).
pub trait SkillEnv {
    /// The user's home directory, or `None` if it cannot be determined.
    fn home_dir(&self) -> Option<PathBuf>;
    /// The current working directory, or `None` if it cannot be determined.
    fn current_dir(&self) -> Option<PathBuf>;
}

/// Production [`SkillEnv`] backed by the process environment.
///
/// `home_dir` reads `HOME`, falling back to `USERPROFILE` (Windows). This
/// matches the workspace's existing `HOME`-keyed path resolution in
/// `mn_core::paths`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdSkillEnv;

impl SkillEnv for StdSkillEnv {
    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }

    fn current_dir(&self) -> Option<PathBuf> {
        std::env::current_dir().ok()
    }
}
```

- [ ] **Step 5: Write `crates/mn-skills/src/lib.rs` with the embed + frontmatter test**

```rust
//! `mn-skills` — the embedded advanced-search `SKILL.md` plus the harness
//! detection, path-resolution, and idempotent install logic shared by the
//! `mnm skills` CLI noun and the `install_search_skill` MCP tool.

#![doc(html_root_url = "https://docs.rs/mn-skills/0.1.0")]

pub mod env;

pub use env::{SkillEnv, StdSkillEnv};

/// The skill's folder name and frontmatter `name` (open Agent Skills standard
/// requires the two to match).
pub const SKILL_NAME: &str = "midnight-advanced-search";

/// The canonical `SKILL.md` body, embedded at build time. This is the single
/// source of truth written into every harness.
#[must_use]
pub const fn skill_markdown() -> &'static str {
    include_str!("../assets/midnight-advanced-search/SKILL.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the `---`-delimited YAML frontmatter at the top of the embedded
    /// `SKILL.md`. Returns the frontmatter block (between the first two `---`
    /// lines).
    fn frontmatter() -> String {
        let md = skill_markdown();
        let mut lines = md.lines();
        assert_eq!(lines.next(), Some("---"), "SKILL.md must open with `---`");
        let mut block = String::new();
        for line in lines {
            if line == "---" {
                return block;
            }
            block.push_str(line);
            block.push('\n');
        }
        panic!("SKILL.md frontmatter not closed with `---`");
    }

    #[derive(serde::Deserialize)]
    struct Frontmatter {
        name: String,
        description: String,
    }

    #[test]
    fn frontmatter_is_valid_and_name_matches_folder() {
        let fm: Frontmatter = serde_yaml::from_str(&frontmatter()).expect("frontmatter parses");
        assert_eq!(fm.name, SKILL_NAME, "frontmatter name must equal SKILL_NAME");
        assert!(!fm.description.trim().is_empty(), "description must be non-empty");
        assert!(
            fm.description.chars().count() <= 1024,
            "description must be <= 1024 chars (open-standard cap); was {}",
            fm.description.chars().count()
        );
    }

    #[test]
    fn name_matches_open_standard_regex() {
        // ^[a-z0-9]+(-[a-z0-9]+)*$ — lowercase alnum, single-hyphen separated.
        let ok = SKILL_NAME
            .split('-')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert!(ok, "SKILL_NAME `{SKILL_NAME}` violates the open-standard name regex");
    }

    #[test]
    fn body_links_the_cookbook_for_dryness() {
        assert!(
            skill_markdown().contains("docs/cookbook/query-enhancement.md"),
            "SKILL.md must link the cookbook (DRY) rather than duplicate worked examples"
        );
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p mn-skills`
Expected: PASS (3 tests). If `serde_yaml` rejects the frontmatter, fix the `SKILL.md` frontmatter, not the test.

- [ ] **Step 7: Run the gate**

Run: `cargo fmt --check && cargo clippy -p mn-skills --all-targets -- -D warnings && cargo test -p mn-skills`
Expected: PASS, no warnings.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/mn-skills
git commit -m "feat(mn-skills): crate skeleton, embedded SKILL.md, SkillEnv"
```

---

## Task 2: Harness registry — `Harness`, `Scope`, paths, markers, reload steps

**Files:**
- Create: `crates/mn-skills/src/harness.rs`
- Modify: `crates/mn-skills/src/lib.rs` (add `pub mod harness;` + re-exports)

- [ ] **Step 1: Write the failing tests** — create `crates/mn-skills/src/harness.rs` with this body (impl stubs come next):

```rust
//! The harness registry: the four supported AI harnesses, the two install
//! scopes, and — per (harness, scope) — the skills-root directory, detection
//! markers, and the reload instruction shown to the user.

use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Install scope, mirroring how each harness scopes skills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// All the user's projects (home-rooted).
    User,
    /// This repository only (repo-root-rooted, committed).
    Project,
}

impl Scope {
    /// Wire / display string (`"user"` or `"project"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

impl FromStr for Scope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            other => Err(other.to_owned()),
        }
    }
}

/// A supported AI harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// Anthropic Claude Code.
    ClaudeCode,
    /// OpenAI Codex CLI.
    Codex,
    /// OpenCode.
    OpenCode,
    /// Cursor (2.4+ native Agent Skills).
    Cursor,
}

impl Harness {
    /// Every supported harness, in display order.
    pub const ALL: [Self; 4] = [Self::ClaudeCode, Self::Codex, Self::OpenCode, Self::Cursor];

    /// The stable id used on the CLI / MCP wire (`claude-code`, `codex`,
    /// `opencode`, `cursor`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
        }
    }

    /// Human-readable name for CLI output.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
        }
    }

    /// The directory holding per-skill folders for this harness at `scope`,
    /// rooted at `base` (home dir for [`Scope::User`], repo root for
    /// [`Scope::Project`]).
    #[must_use]
    pub fn skills_root(self, scope: Scope, base: &Path) -> PathBuf {
        match (self, scope) {
            (Self::ClaudeCode, _) => base.join(".claude").join("skills"),
            (Self::Codex, _) => base.join(".agents").join("skills"),
            (Self::OpenCode, Scope::User) => {
                base.join(".config").join("opencode").join("skills")
            }
            (Self::OpenCode, Scope::Project) => base.join(".opencode").join("skills"),
            (Self::Cursor, _) => base.join(".cursor").join("skills"),
        }
    }

    /// The owned skill directory (`<skills_root>/midnight-advanced-search`).
    #[must_use]
    pub fn skill_dir(self, scope: Scope, base: &Path) -> PathBuf {
        self.skills_root(scope, base).join(crate::SKILL_NAME)
    }

    /// The installed `SKILL.md` path.
    #[must_use]
    pub fn skill_file(self, scope: Scope, base: &Path) -> PathBuf {
        self.skill_dir(scope, base).join("SKILL.md")
    }

    /// Detection markers for this harness at `scope`, rooted at `base`. The
    /// harness is considered present if ANY marker exists.
    #[must_use]
    pub fn markers(self, scope: Scope, base: &Path) -> Vec<PathBuf> {
        match (self, scope) {
            (Self::ClaudeCode, _) => vec![base.join(".claude")],
            (Self::Codex, Scope::User) => vec![base.join(".codex"), base.join(".agents")],
            (Self::Codex, Scope::Project) => {
                vec![base.join(".codex"), base.join(".agents"), base.join("AGENTS.md")]
            }
            (Self::OpenCode, Scope::User) => vec![base.join(".config").join("opencode")],
            (Self::OpenCode, Scope::Project) => vec![base.join(".opencode")],
            (Self::Cursor, _) => vec![base.join(".cursor")],
        }
    }

    /// The per-harness "reload your skills" instruction to print / relay.
    #[must_use]
    pub const fn reload_step(self) -> &'static str {
        match self {
            Self::ClaudeCode => {
                "Claude Code auto-discovers skills. If its skills directory was created just now, \
                 restart Claude Code; otherwise it loads live. Verify by asking \"what skills are available?\"."
            }
            Self::Codex => {
                "Codex auto-detects new skills. If it doesn't appear, restart Codex (or run /skills to list)."
            }
            Self::OpenCode => "Restart your OpenCode session to load the new skill.",
            Self::Cursor => "Cursor discovers skills on startup. Restart Cursor to load the new skill.",
        }
    }
}

impl FromStr for Harness {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-code" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::OpenCode),
            "cursor" => Ok(Self::Cursor),
            other => Err(other.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn id_round_trips() {
        for h in Harness::ALL {
            assert_eq!(Harness::from_str(h.id()), Ok(h));
        }
    }

    #[test]
    fn unknown_id_is_err() {
        assert!(Harness::from_str("windsurf").is_err());
    }

    #[test]
    fn user_paths_match_verified_matrix() {
        let home = Path::new("/home/u");
        assert_eq!(
            Harness::ClaudeCode.skill_file(Scope::User, home),
            Path::new("/home/u/.claude/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::Codex.skill_file(Scope::User, home),
            Path::new("/home/u/.agents/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::OpenCode.skill_file(Scope::User, home),
            Path::new("/home/u/.config/opencode/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::Cursor.skill_file(Scope::User, home),
            Path::new("/home/u/.cursor/skills/midnight-advanced-search/SKILL.md")
        );
    }

    #[test]
    fn project_paths_match_verified_matrix() {
        let root = Path::new("/repo");
        assert_eq!(
            Harness::ClaudeCode.skill_file(Scope::Project, root),
            Path::new("/repo/.claude/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::Codex.skill_file(Scope::Project, root),
            Path::new("/repo/.agents/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::OpenCode.skill_file(Scope::Project, root),
            Path::new("/repo/.opencode/skills/midnight-advanced-search/SKILL.md")
        );
        assert_eq!(
            Harness::Cursor.skill_file(Scope::Project, root),
            Path::new("/repo/.cursor/skills/midnight-advanced-search/SKILL.md")
        );
    }

    #[test]
    fn scope_round_trips() {
        assert_eq!(Scope::from_str("user"), Ok(Scope::User));
        assert_eq!(Scope::from_str("project"), Ok(Scope::Project));
        assert!(Scope::from_str("global").is_err());
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

In `crates/mn-skills/src/lib.rs`, after `pub mod env;` add:

```rust
pub mod harness;

pub use harness::{Harness, Scope};
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p mn-skills harness`
Expected: PASS (all `harness::tests`). The module compiles because the impls are written alongside the tests; verify the path-assertion tests pass exactly.

- [ ] **Step 4: Run the gate**

Run: `cargo fmt --check && cargo clippy -p mn-skills --all-targets -- -D warnings && cargo test -p mn-skills`
Expected: PASS, no warnings. (Watch for clippy wanting `#[must_use]` / `const fn` — already applied.)

- [ ] **Step 5: Commit**

```bash
git add crates/mn-skills/src/harness.rs crates/mn-skills/src/lib.rs
git commit -m "feat(mn-skills): harness registry — paths, markers, reload steps"
```

---

## Task 3: Detection + repo-root walk + base-dir resolution

**Files:**
- Create: `crates/mn-skills/src/error.rs` (the shared `SkillError`, used by both `detect` and `install`)
- Create: `crates/mn-skills/src/detect.rs`
- Modify: `crates/mn-skills/src/lib.rs` (`pub mod detect;` + `pub mod error;` + re-exports)

- [ ] **Step 1: Write `crates/mn-skills/src/error.rs`**

```rust
//! The crate's error type, shared by detection and install logic.

use std::path::PathBuf;

/// Anything that can go wrong resolving paths or writing the skill.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// Neither `HOME` nor `USERPROFILE` is set, so user-scope paths are
    /// unresolvable.
    #[error("could not determine home directory (HOME / USERPROFILE unset)")]
    NoHome,
    /// The current working directory could not be read, so project-scope paths
    /// are unresolvable.
    #[error("could not determine the current working directory")]
    NoCwd,
    /// Auto-detect found no supported harness and none were forced.
    #[error(
        "no supported AI harness detected at {scope} scope (probed: {probed}); \
         pass --harness with one or more of: claude-code, codex, opencode, cursor"
    )]
    NoHarnessDetected {
        /// The scope that was probed.
        scope: String,
        /// Comma-joined harness ids that were probed.
        probed: String,
    },
    /// A filesystem write / read / delete failed.
    #[error("filesystem error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying io error.
        #[source]
        source: std::io::Error,
    },
}
```

- [ ] **Step 2: Write the failing tests + impl** — create `crates/mn-skills/src/detect.rs`:

```rust
//! Base-directory resolution (home for user scope, repo root for project
//! scope) and marker-based harness detection.

use std::path::{Path, PathBuf};

use crate::error::SkillError;
use crate::harness::{Harness, Scope};
use crate::SkillEnv;

/// The directory all paths for `scope` are rooted at: the home dir for
/// [`Scope::User`], the repository root (walked up from cwd) for
/// [`Scope::Project`].
///
/// # Errors
///
/// [`SkillError::NoHome`] / [`SkillError::NoCwd`] when the environment can't
/// supply the anchor.
pub fn base_dir(scope: Scope, env: &impl SkillEnv) -> Result<PathBuf, SkillError> {
    match scope {
        Scope::User => env.home_dir().ok_or(SkillError::NoHome),
        Scope::Project => {
            let cwd = env.current_dir().ok_or(SkillError::NoCwd)?;
            Ok(repo_root(&cwd))
        }
    }
}

/// Walk up from `start` to the nearest ancestor containing a `.git` entry.
/// Falls back to `start` itself when no `.git` is found.
fn repo_root(start: &Path) -> PathBuf {
    let mut cur: &Path = start;
    loop {
        if cur.join(".git").exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => return start.to_path_buf(),
        }
    }
}

/// Detect which harnesses are present at `scope`. A harness is present when any
/// of its markers exists under the resolved base dir.
///
/// # Errors
///
/// Propagates [`base_dir`] errors.
pub fn detect(scope: Scope, env: &impl SkillEnv) -> Result<Vec<Harness>, SkillError> {
    let base = base_dir(scope, env)?;
    Ok(Harness::ALL
        .into_iter()
        .filter(|h| h.markers(scope, &base).iter().any(|m| m.exists()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct FakeEnv {
        home: PathBuf,
        cwd: PathBuf,
    }
    impl SkillEnv for FakeEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn current_dir(&self) -> Option<PathBuf> {
            Some(self.cwd.clone())
        }
    }

    #[test]
    fn detect_user_scope_by_marker() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".cursor")).unwrap();
        let env = FakeEnv { home: home.clone(), cwd: home.clone() };
        let mut got = detect(Scope::User, &env).unwrap();
        got.sort_by_key(|h| h.id());
        assert_eq!(got, vec![Harness::ClaudeCode, Harness::Cursor]);
    }

    #[test]
    fn detect_none_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf(), cwd: tmp.path().to_path_buf() };
        assert!(detect(Scope::User, &env).unwrap().is_empty());
    }

    #[test]
    fn project_base_walks_up_to_git_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("crates").join("x");
        fs::create_dir_all(&nested).unwrap();
        let env = FakeEnv { home: root.to_path_buf(), cwd: nested };
        assert_eq!(base_dir(Scope::Project, &env).unwrap(), root);
    }

    #[test]
    fn project_base_falls_back_to_cwd_without_git() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().join("loose");
        fs::create_dir_all(&cwd).unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf(), cwd: cwd.clone() };
        assert_eq!(base_dir(Scope::Project, &env).unwrap(), cwd);
    }

    #[test]
    fn detect_codex_project_via_agents_md() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("AGENTS.md"), "x").unwrap();
        let env = FakeEnv { home: root.to_path_buf(), cwd: root.to_path_buf() };
        assert_eq!(detect(Scope::Project, &env).unwrap(), vec![Harness::Codex]);
    }
}
```

- [ ] **Step 3: Wire modules into `lib.rs`**

In `crates/mn-skills/src/lib.rs`, add after the existing module declarations:

```rust
pub mod detect;
pub mod error;

pub use detect::{base_dir, detect};
pub use error::SkillError;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p mn-skills detect`
Expected: PASS (5 detect tests).

- [ ] **Step 5: Run the gate**

Run: `cargo fmt --check && cargo clippy -p mn-skills --all-targets -- -D warnings && cargo test -p mn-skills`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/mn-skills/src/error.rs crates/mn-skills/src/detect.rs crates/mn-skills/src/lib.rs
git commit -m "feat(mn-skills): marker detection + repo-root base-dir resolution"
```

---

## Task 4: Install / remove / status + report types

**Files:**
- Create: `crates/mn-skills/src/install.rs`
- Modify: `crates/mn-skills/src/lib.rs` (`pub mod install;` + re-exports)

- [ ] **Step 1: Write the failing tests + impl** — create `crates/mn-skills/src/install.rs`:

```rust
//! Idempotent install / remove / status over the owned
//! `midnight-advanced-search/` directory.

use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::detect::{base_dir, detect};
use crate::error::SkillError;
use crate::harness::{Harness, Scope};
use crate::{skill_markdown, SkillEnv, SKILL_NAME};

/// What an install did to a single harness's owned dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallAction {
    /// The skill file did not exist; it was written.
    Created,
    /// The skill file existed with different content; it was overwritten.
    Updated,
    /// The skill file existed with byte-identical content; no write.
    Unchanged,
}

/// Per-harness install outcome.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessInstall {
    /// Harness id (`claude-code`, …).
    pub harness: String,
    /// Scope (`user` / `project`).
    pub scope: String,
    /// The `SKILL.md` path written.
    pub path: PathBuf,
    /// What happened.
    pub action: InstallAction,
    /// The "reload your skills" instruction for this harness.
    pub reload_step: String,
}

/// Result of an [`install`] call.
#[derive(Debug, Clone, Serialize)]
pub struct InstallReport {
    /// The installed skill's name.
    pub skill_name: String,
    /// Scope all writes targeted.
    pub scope: String,
    /// One entry per harness written.
    pub installed: Vec<HarnessInstall>,
    /// Harness ids probed but not detected (empty when `--harness` was forced).
    pub not_detected: Vec<String>,
}

/// Per-harness removal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveAction {
    /// The owned dir existed and was deleted.
    Removed,
    /// The owned dir did not exist.
    Absent,
}

/// One harness's removal result.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessRemove {
    /// Harness id.
    pub harness: String,
    /// Scope.
    pub scope: String,
    /// The owned dir targeted.
    pub path: PathBuf,
    /// What happened.
    pub action: RemoveAction,
}

/// Result of a [`remove`] call.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveReport {
    /// The skill's name.
    pub skill_name: String,
    /// Scope.
    pub scope: String,
    /// One entry per harness targeted.
    pub removed: Vec<HarnessRemove>,
}

/// One harness's status at a scope.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessStatus {
    /// Harness id.
    pub harness: String,
    /// Scope.
    pub scope: String,
    /// Whether the harness's marker is present.
    pub detected: bool,
    /// Whether our `SKILL.md` is installed.
    pub installed: bool,
    /// Whether the installed copy is byte-identical to the embedded skill.
    /// `false` when not installed or when it differs (stale / user-edited).
    pub up_to_date: bool,
    /// The resolved `SKILL.md` path.
    pub path: PathBuf,
}

/// Result of a [`status`] call.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    /// The skill's name.
    pub skill_name: String,
    /// Scope.
    pub scope: String,
    /// One entry per supported harness.
    pub harnesses: Vec<HarnessStatus>,
}

/// Resolve which harnesses to act on:
/// - `Some(list)` → exactly those (forced; detection skipped, `not_detected`
///   empty).
/// - `None` → auto-detect; errors [`SkillError::NoHarnessDetected`] if none.
///
/// Returns the targets plus the ids that were probed-but-absent (only
/// meaningful in the auto-detect branch).
fn resolve_targets(
    explicit: Option<&[Harness]>,
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<(Vec<Harness>, Vec<String>), SkillError> {
    if let Some(list) = explicit {
        return Ok((list.to_vec(), Vec::new()));
    }
    let detected = detect(scope, env)?;
    if detected.is_empty() {
        return Err(SkillError::NoHarnessDetected {
            scope: scope.as_str().to_owned(),
            probed: Harness::ALL.iter().map(|h| h.id()).collect::<Vec<_>>().join(", "),
        });
    }
    let not_detected = Harness::ALL
        .into_iter()
        .filter(|h| !detected.contains(h))
        .map(|h| h.id().to_owned())
        .collect();
    Ok((detected, not_detected))
}

/// Install the embedded skill for `explicit` harnesses (or auto-detected ones),
/// idempotently, at `scope`.
///
/// # Errors
///
/// [`SkillError::NoHarnessDetected`] when auto-detect finds nothing,
/// path-resolution errors, or [`SkillError::Io`] on a failed write.
pub fn install(
    explicit: Option<&[Harness]>,
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<InstallReport, SkillError> {
    let base = base_dir(scope, env)?;
    let (targets, not_detected) = resolve_targets(explicit, scope, env)?;
    let body = skill_markdown();
    let mut installed = Vec::with_capacity(targets.len());
    for h in targets {
        let dir = h.skill_dir(scope, &base);
        let file = dir.join("SKILL.md");
        let action = match fs::read_to_string(&file) {
            Ok(existing) if existing == body => InstallAction::Unchanged,
            Ok(_) => {
                write_file(&file, body)?;
                InstallAction::Updated
            }
            Err(_) => {
                fs::create_dir_all(&dir).map_err(|source| SkillError::Io {
                    path: dir.clone(),
                    source,
                })?;
                write_file(&file, body)?;
                InstallAction::Created
            }
        };
        installed.push(HarnessInstall {
            harness: h.id().to_owned(),
            scope: scope.as_str().to_owned(),
            path: file,
            action,
            reload_step: h.reload_step().to_owned(),
        });
    }
    Ok(InstallReport {
        skill_name: SKILL_NAME.to_owned(),
        scope: scope.as_str().to_owned(),
        installed,
        not_detected,
    })
}

fn write_file(path: &std::path::Path, body: &str) -> Result<(), SkillError> {
    fs::write(path, body).map_err(|source| SkillError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Remove the owned skill dir for `explicit` harnesses (or auto-detected ones)
/// at `scope`.
///
/// # Errors
///
/// As [`install`], plus [`SkillError::Io`] on a failed delete.
pub fn remove(
    explicit: Option<&[Harness]>,
    scope: Scope,
    env: &impl SkillEnv,
) -> Result<RemoveReport, SkillError> {
    let base = base_dir(scope, env)?;
    let (targets, _) = resolve_targets(explicit, scope, env)?;
    let mut removed = Vec::with_capacity(targets.len());
    for h in targets {
        let dir = h.skill_dir(scope, &base);
        let action = if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|source| SkillError::Io {
                path: dir.clone(),
                source,
            })?;
            RemoveAction::Removed
        } else {
            RemoveAction::Absent
        };
        removed.push(HarnessRemove {
            harness: h.id().to_owned(),
            scope: scope.as_str().to_owned(),
            path: dir,
            action,
        });
    }
    Ok(RemoveReport {
        skill_name: SKILL_NAME.to_owned(),
        scope: scope.as_str().to_owned(),
        removed,
    })
}

/// Report detection + install state for every supported harness at `scope`.
/// Never errors on "nothing detected" — only on an unresolvable base dir.
///
/// # Errors
///
/// Path-resolution errors only.
pub fn status(scope: Scope, env: &impl SkillEnv) -> Result<StatusReport, SkillError> {
    let base = base_dir(scope, env)?;
    let body = skill_markdown();
    let harnesses = Harness::ALL
        .into_iter()
        .map(|h| {
            let file = h.skill_file(scope, &base);
            let detected = h.markers(scope, &base).iter().any(|m| m.exists());
            let installed_content = fs::read_to_string(&file).ok();
            let installed = installed_content.is_some();
            let up_to_date = installed_content.as_deref() == Some(body);
            HarnessStatus {
                harness: h.id().to_owned(),
                scope: scope.as_str().to_owned(),
                detected,
                installed,
                up_to_date,
                path: file,
            }
        })
        .collect();
    Ok(StatusReport {
        skill_name: SKILL_NAME.to_owned(),
        scope: scope.as_str().to_owned(),
        harnesses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct FakeEnv {
        home: PathBuf,
    }
    impl SkillEnv for FakeEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn current_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
    }

    fn env_with_marker(harness: Harness) -> (TempDir, FakeEnv) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        for m in harness.markers(Scope::User, &home) {
            // create the first marker as a dir
            std::fs::create_dir_all(&m).unwrap();
            break;
        }
        let env = FakeEnv { home };
        (tmp, env)
    }

    #[test]
    fn install_then_reinstall_is_idempotent() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let first = install(None, Scope::User, &env).unwrap();
        assert_eq!(first.installed.len(), 1);
        assert_eq!(first.installed[0].action, InstallAction::Created);
        assert!(first.installed[0].path.exists());

        let second = install(None, Scope::User, &env).unwrap();
        assert_eq!(second.installed[0].action, InstallAction::Unchanged);
    }

    #[test]
    fn install_overwrites_stale_content_as_updated() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        let path = report.installed[0].path.clone();
        std::fs::write(&path, "stale body").unwrap();

        let again = install(None, Scope::User, &env).unwrap();
        assert_eq!(again.installed[0].action, InstallAction::Updated);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), skill_markdown());
    }

    #[test]
    fn explicit_harness_forces_install_even_when_undetected() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        let report = install(Some(&[Harness::Cursor]), Scope::User, &env).unwrap();
        assert_eq!(report.installed.len(), 1);
        assert_eq!(report.installed[0].harness, "cursor");
        assert!(report.not_detected.is_empty());
    }

    #[test]
    fn autodetect_with_no_harness_errors() {
        let tmp = TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        let err = install(None, Scope::User, &env).unwrap_err();
        assert!(matches!(err, SkillError::NoHarnessDetected { .. }));
    }

    #[test]
    fn not_detected_lists_absent_harnesses_on_autodetect() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        assert_eq!(report.installed.len(), 1);
        let mut nd = report.not_detected.clone();
        nd.sort();
        assert_eq!(nd, vec!["codex", "cursor", "opencode"]);
    }

    #[test]
    fn status_reports_installed_and_stale() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        install(None, Scope::User, &env).unwrap();
        let st = status(Scope::User, &env).unwrap();
        let cc = st.harnesses.iter().find(|h| h.harness == "claude-code").unwrap();
        assert!(cc.detected && cc.installed && cc.up_to_date);
        let cursor = st.harnesses.iter().find(|h| h.harness == "cursor").unwrap();
        assert!(!cursor.detected && !cursor.installed && !cursor.up_to_date);

        std::fs::write(&cc.path, "stale").unwrap();
        let st2 = status(Scope::User, &env).unwrap();
        let cc2 = st2.harnesses.iter().find(|h| h.harness == "claude-code").unwrap();
        assert!(cc2.installed && !cc2.up_to_date);
    }

    #[test]
    fn remove_deletes_then_reports_absent() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        install(None, Scope::User, &env).unwrap();
        let r1 = remove(Some(&[Harness::ClaudeCode]), Scope::User, &env).unwrap();
        assert_eq!(r1.removed[0].action, RemoveAction::Removed);
        assert!(!r1.removed[0].path.exists());
        let r2 = remove(Some(&[Harness::ClaudeCode]), Scope::User, &env).unwrap();
        assert_eq!(r2.removed[0].action, RemoveAction::Absent);
    }

    #[test]
    fn report_serializes_to_expected_json_shape() {
        let (_tmp, env) = env_with_marker(Harness::ClaudeCode);
        let report = install(None, Scope::User, &env).unwrap();
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["skill_name"], "midnight-advanced-search");
        assert_eq!(v["scope"], "user");
        assert_eq!(v["installed"][0]["harness"], "claude-code");
        assert_eq!(v["installed"][0]["action"], "created");
        assert!(v["installed"][0]["reload_step"].is_string());
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

In `crates/mn-skills/src/lib.rs`, add:

```rust
pub mod install;

pub use install::{
    install, remove, status, HarnessInstall, HarnessRemove, HarnessStatus, InstallAction,
    InstallReport, RemoveAction, RemoveReport, StatusReport,
};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mn-skills install`
Expected: PASS (8 install tests).

- [ ] **Step 4: Run the gate**

Run: `cargo fmt --check && cargo clippy -p mn-skills --all-targets -- -D warnings && cargo test -p mn-skills`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mn-skills/src/install.rs crates/mn-skills/src/lib.rs
git commit -m "feat(mn-skills): idempotent install/remove/status + report types"
```

---

## Task 5: `mnm skills {add,status,remove}` CLI noun

**Files:**
- Create: `crates/mn-cli/src/commands/skills/mod.rs`
- Create: `crates/mn-cli/src/commands/skills/add.rs`
- Create: `crates/mn-cli/src/commands/skills/status.rs`
- Create: `crates/mn-cli/src/commands/skills/remove.rs`
- Modify: `crates/mn-cli/Cargo.toml` (add `mn-skills` dep)
- Modify: `crates/mn-cli/src/commands/mod.rs` (add `pub mod skills;`)
- Modify: `crates/mn-cli/src/cli.rs` (`Command::Skills` variant + dispatch + `cli_command_name`)
- Modify: `crates/mn-telemetry/src/events.rs` (`CliCommandName::Skills`)

- [ ] **Step 1: Add the dependency**

In `crates/mn-cli/Cargo.toml`, under `[dependencies]`, after `mn-retrieval`:

```toml
mn-skills     = { path = "../mn-skills" }
```

- [ ] **Step 2: Add the telemetry variant**

In `crates/mn-telemetry/src/events.rs`, in `enum CliCommandName`, after the `Documents` variant:

```rust
    /// `mnm skills` (any sub).
    Skills,
```

- [ ] **Step 3: Write the `skills` dispatcher with shared parsing** — create `crates/mn-cli/src/commands/skills/mod.rs`:

```rust
//! `mnm skills <subcommand>` — install / inspect / remove the advanced-search
//! skill in the user's AI harness(es).

use anyhow::{anyhow, Result};
use clap::{Args as ClapArgs, Subcommand};
use mn_skills::{Harness, Scope};

pub mod add;
pub mod remove;
pub mod status;

/// Skills namespace arguments.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Subcommand.
    #[command(subcommand)]
    pub cmd: SkillsCmd,
}

/// Skills subcommands.
#[derive(Debug, Subcommand)]
pub enum SkillsCmd {
    /// Install (or update) the advanced-search skill.
    Add(add::Args),
    /// Show where the skill is installed and whether it's current.
    Status(status::Args),
    /// Remove the advanced-search skill.
    Remove(remove::Args),
}

/// Dispatcher for the skills namespace.
///
/// # Errors
///
/// Propagates subcommand failures (bad `--harness` / `--scope`, install IO).
pub fn run(args: Args, json: bool) -> Result<()> {
    match args.cmd {
        SkillsCmd::Add(a) => add::run(a, json),
        SkillsCmd::Status(a) => status::run(a, json),
        SkillsCmd::Remove(a) => remove::run(a, json),
    }
}

/// Parse the optional `--harness a,b,c` flag into harnesses. `None` (flag
/// omitted) means auto-detect; an empty string is rejected.
pub(super) fn parse_harnesses(raw: Option<&str>) -> Result<Option<Vec<Harness>>> {
    let Some(raw) = raw else { return Ok(None) };
    let mut out = Vec::new();
    for tok in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let h = tok
            .parse::<Harness>()
            .map_err(|bad| anyhow!("unknown harness `{bad}` (expected: claude-code, codex, opencode, cursor)"))?;
        if !out.contains(&h) {
            out.push(h);
        }
    }
    if out.is_empty() {
        return Err(anyhow!("--harness was empty; give one or more of: claude-code, codex, opencode, cursor"));
    }
    Ok(Some(out))
}

/// Parse the `--scope` flag (default `user`).
pub(super) fn parse_scope(raw: &str) -> Result<Scope> {
    raw.parse::<Scope>()
        .map_err(|bad| anyhow!("unknown scope `{bad}` (expected: user, project)"))
}
```

- [ ] **Step 4: Write `add.rs`** — create `crates/mn-cli/src/commands/skills/add.rs`:

```rust
//! `mnm skills add` — install the advanced-search skill into detected (or
//! specified) harnesses.

use anyhow::Result;
use clap::Args as ClapArgs;
use mn_skills::StdSkillEnv;

/// Arguments for `mnm skills add`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Comma-separated harnesses (`claude-code`, `codex`, `opencode`,
    /// `cursor`). Omit to auto-detect installed harnesses.
    #[arg(long)]
    pub harness: Option<String>,
    /// Install scope: `user` (all your projects) or `project` (this repo).
    #[arg(long, default_value = "user")]
    pub scope: String,
}

/// Run `mnm skills add`.
///
/// # Errors
///
/// Bad flags, no harness detected, or a filesystem write failure.
pub fn run(args: Args, json: bool) -> Result<()> {
    let targets = super::parse_harnesses(args.harness.as_deref())?;
    let scope = super::parse_scope(&args.scope)?;
    let report = mn_skills::install(targets.as_deref(), scope, &StdSkillEnv)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Installed `{}` at {} scope:\n", report.skill_name, report.scope);
    for h in &report.installed {
        let verb = match h.action {
            mn_skills::InstallAction::Created => "created",
            mn_skills::InstallAction::Updated => "updated",
            mn_skills::InstallAction::Unchanged => "already current",
        };
        println!("  {} — {verb}", h.harness);
        println!("    path:   {}", h.path.display());
        println!("    reload: {}\n", h.reload_step);
    }
    if !report.not_detected.is_empty() {
        println!("Not detected (skipped): {}", report.not_detected.join(", "));
        println!("Force one with: mnm skills add --harness <name>");
    }
    Ok(())
}
```

- [ ] **Step 5: Write `status.rs`** — create `crates/mn-cli/src/commands/skills/status.rs`:

```rust
//! `mnm skills status` — show install state per harness.

use anyhow::Result;
use clap::Args as ClapArgs;
use mn_skills::StdSkillEnv;

/// Arguments for `mnm skills status`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Scope to inspect: `user` or `project`.
    #[arg(long, default_value = "user")]
    pub scope: String,
}

/// Run `mnm skills status`.
///
/// # Errors
///
/// Bad `--scope`, or an unresolvable home / cwd.
pub fn run(args: Args, json: bool) -> Result<()> {
    let scope = super::parse_scope(&args.scope)?;
    let report = mn_skills::status(scope, &StdSkillEnv)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("`{}` at {} scope:\n", report.skill_name, report.scope);
    println!("  {:<12}  {:<9}  {:<10}  {}", "harness", "detected", "installed", "state");
    for h in &report.harnesses {
        let state = if !h.installed {
            "—"
        } else if h.up_to_date {
            "up to date"
        } else {
            "stale"
        };
        println!(
            "  {:<12}  {:<9}  {:<10}  {}",
            h.harness,
            yes_no(h.detected),
            yes_no(h.installed),
            state
        );
    }
    Ok(())
}

const fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}
```

- [ ] **Step 6: Write `remove.rs`** — create `crates/mn-cli/src/commands/skills/remove.rs`:

```rust
//! `mnm skills remove` — delete the advanced-search skill.

use anyhow::Result;
use clap::Args as ClapArgs;
use mn_skills::StdSkillEnv;

/// Arguments for `mnm skills remove`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Comma-separated harnesses to remove from. Omit to auto-detect.
    #[arg(long)]
    pub harness: Option<String>,
    /// Scope: `user` or `project`.
    #[arg(long, default_value = "user")]
    pub scope: String,
}

/// Run `mnm skills remove`.
///
/// # Errors
///
/// Bad flags, no harness detected, or a filesystem delete failure.
pub fn run(args: Args, json: bool) -> Result<()> {
    let targets = super::parse_harnesses(args.harness.as_deref())?;
    let scope = super::parse_scope(&args.scope)?;
    let report = mn_skills::remove(targets.as_deref(), scope, &StdSkillEnv)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Removed `{}` at {} scope:\n", report.skill_name, report.scope);
    for h in &report.removed {
        let verb = match h.action {
            mn_skills::RemoveAction::Removed => "removed",
            mn_skills::RemoveAction::Absent => "not installed",
        };
        println!("  {} — {verb} ({})", h.harness, h.path.display());
    }
    Ok(())
}
```

- [ ] **Step 7: Register the module + command**

In `crates/mn-cli/src/commands/mod.rs`, add `pub mod skills;` (keep alphabetical / grouped with the other nouns).

In `crates/mn-cli/src/cli.rs`:

In `enum Command`, after the `Documents` variant:

```rust
    /// Install the advanced-search skill into your AI harness(es).
    Skills(commands::skills::Args),
```

In `run()`'s `match cli.cmd`, after the `Command::Documents` arm:

```rust
        Command::Skills(args) => commands::skills::run(args, cli.json),
```

> Note: `commands::skills::run` is synchronous and returns `Result<()>`. The surrounding arms are `.await`ed futures; a bare `Ok(())`-returning call is fine in the `match` because each arm's value is the `Result` assigned to `result`. Do NOT add `.await`.

In `cli_command_name()`'s match, after the `Command::Documents(_)` arm:

```rust
        Command::Skills(_) => CliCommandName::Skills,
```

- [ ] **Step 8: Write a CLI parse test** — append to `crates/mn-cli/src/commands/skills/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_harnesses_dedupes_and_validates() {
        let got = parse_harnesses(Some("claude-code,cursor,claude-code")).unwrap().unwrap();
        assert_eq!(got, vec![Harness::ClaudeCode, Harness::Cursor]);
    }

    #[test]
    fn parse_harnesses_none_is_autodetect() {
        assert!(parse_harnesses(None).unwrap().is_none());
    }

    #[test]
    fn parse_harnesses_rejects_unknown() {
        assert!(parse_harnesses(Some("windsurf")).is_err());
    }

    #[test]
    fn parse_scope_defaults_and_rejects() {
        assert_eq!(parse_scope("user").unwrap(), Scope::User);
        assert_eq!(parse_scope("project").unwrap(), Scope::Project);
        assert!(parse_scope("global").is_err());
    }
}
```

- [ ] **Step 9: Run tests**

Run: `cargo test -p mn-cli skills && cargo test -p mn-telemetry`
Expected: PASS.

- [ ] **Step 10: Smoke-test the binary against a temp HOME**

Run:
```bash
TMP=$(mktemp -d); mkdir -p "$TMP/.claude"; HOME="$TMP" cargo run -q -p mn-cli --bin mnm -- skills add --harness claude-code
cat "$TMP/.claude/skills/midnight-advanced-search/SKILL.md" | head -3
HOME="$TMP" cargo run -q -p mn-cli --bin mnm -- skills status
```
Expected: prints "created" with the path; the file's first line is `---`; status shows `claude-code  yes  yes  up to date`.

- [ ] **Step 11: Run the gate**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS, no warnings.

- [ ] **Step 12: Commit**

```bash
git add crates/mn-cli crates/mn-telemetry/src/events.rs
git commit -m "feat(mn-cli): mnm skills add/status/remove noun"
```

---

## Task 6: `install_search_skill` MCP tool

**Files:**
- Modify: `crates/mn-mcp/Cargo.toml` (add `mn-skills` dep)
- Modify: `crates/mn-mcp/src/tools.rs` (manifest entry + `run_install_search_skill`)
- Modify: `crates/mn-mcp/src/server.rs` (dispatch arm + `tool_name_for_event`)
- Modify: `crates/mn-mcp/src/lib.rs` (doc: 11 → 12 tools)
- Modify: `crates/mn-telemetry/src/events.rs` (`McpToolName::InstallSearchSkill`)
- Modify: `specs/001-rag-platform/contracts/mcp-tools.json` (tool entry + version bump)

- [ ] **Step 1: Add the dependency**

In `crates/mn-mcp/Cargo.toml`, under `[dependencies]`, after `mn-retrieval`:

```toml
mn-skills     = { path = "../mn-skills" }
```

- [ ] **Step 2: Add the telemetry variant**

In `crates/mn-telemetry/src/events.rs`, in `enum McpToolName`, after the `Status` variant:

```rust
    /// `install_search_skill` tool.
    InstallSearchSkill,
```

- [ ] **Step 3: Add the manifest entry + run fn in `tools.rs`**

In `crates/mn-mcp/src/tools.rs`, inside `list()`'s `vec![...]`, after the `status` `ToolDescription` (before the closing `]`):

```rust
            ToolDescription {
                name: "install_search_skill",
                description:
                    "Install the midnight-advanced-search Agent Skill (a persistent retrieval playbook) into the user's AI harness(es). Writes the same SKILL.md to each detected harness's native skills directory; re-running updates in place. Returns, per harness, the scope, the exact path written, the action (created/updated/unchanged), and the reload step to relay to the user. Optional `harness` (subset of claude-code/codex/opencode/cursor) forces specific targets; omit to auto-detect. Optional `scope` is user (default) or project.",
                input_schema: install_search_skill_schema(),
            },
```

Add the schema helper and run fn near the other `*_schema` helpers in `tools.rs`:

```rust
fn install_search_skill_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "harness": {
                "type": "array",
                "items": { "type": "string", "enum": ["claude-code", "codex", "opencode", "cursor"] },
                "description": "Harnesses to install for. Omit to auto-detect."
            },
            "scope": {
                "type": "string",
                "enum": ["user", "project"],
                "default": "user",
                "description": "Install scope."
            }
        },
        "additionalProperties": false,
    })
}

/// Parse the tool arguments and run the install against the real process
/// environment. Returns the JSON report as a string on success, or an
/// `(ErrorCode, message)` pair the dispatcher turns into a JSON-RPC error.
///
/// # Errors
///
/// Returns `InvalidParams` for a bad `harness`/`scope`, `ToolFailed` for a
/// filesystem failure or no-harness-detected.
pub fn run_install_search_skill(
    args: &serde_json::Value,
) -> Result<String, (crate::protocol::ErrorCode, String)> {
    run_install_search_skill_in(args, &mn_skills::StdSkillEnv)
}

/// Inner form that takes the [`mn_skills::SkillEnv`] explicitly, so tests can
/// inject a fake home/cwd instead of mutating the global `HOME`.
///
/// # Errors
///
/// As [`run_install_search_skill`].
pub fn run_install_search_skill_in(
    args: &serde_json::Value,
    env: &impl mn_skills::SkillEnv,
) -> Result<String, (crate::protocol::ErrorCode, String)> {
    use crate::protocol::ErrorCode;
    use mn_skills::{Harness, Scope};
    use std::str::FromStr as _;

    let scope = match args.get("scope") {
        None => Scope::User,
        Some(serde_json::Value::String(s)) => Scope::from_str(s)
            .map_err(|bad| (ErrorCode::InvalidParams, format!("unknown scope `{bad}`")))?,
        Some(_) => return Err((ErrorCode::InvalidParams, "scope must be a string".to_owned())),
    };

    let explicit: Option<Vec<Harness>> = match args.get("harness") {
        None => None,
        Some(serde_json::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let s = item.as_str().ok_or((
                    ErrorCode::InvalidParams,
                    "harness entries must be strings".to_owned(),
                ))?;
                let h = Harness::from_str(s)
                    .map_err(|bad| (ErrorCode::InvalidParams, format!("unknown harness `{bad}`")))?;
                if !out.contains(&h) {
                    out.push(h);
                }
            }
            if out.is_empty() {
                return Err((ErrorCode::InvalidParams, "harness array was empty".to_owned()));
            }
            Some(out)
        }
        Some(_) => return Err((ErrorCode::InvalidParams, "harness must be an array".to_owned())),
    };

    let report = mn_skills::install(explicit.as_deref(), scope, env)
        .map_err(|e| (ErrorCode::ToolFailed, e.to_string()))?;
    serde_json::to_string(&report)
        .map_err(|e| (ErrorCode::ToolFailed, format!("serialize report: {e}")))
}
```

- [ ] **Step 4: Add the dispatch arm + telemetry name in `server.rs`**

In `crates/mn-mcp/src/server.rs`, in `dispatch_tool_inner`'s `match params.name.as_str()`, add an arm (after `"list_sources" => ...`):

```rust
        "install_search_skill" => match tools::run_install_search_skill(&params.arguments) {
            Ok(text) => Ok(text),
            Err((code, msg)) => Err(Response::err(id.clone(), code, msg)),
        },
```

In `tool_name_for_event`, add before the `_ => None` arm:

```rust
        "install_search_skill" => Some(McpToolName::InstallSearchSkill),
```

- [ ] **Step 5: Update the tool-count doc in `lib.rs`**

In `crates/mn-mcp/src/lib.rs`, change the doc comment "Eleven tools across three categories" to "Twelve tools across four categories" and add a line under the categories list:

```rust
//! - Local install: `install_search_skill` (writes the advanced-search
//!   SKILL.md into the user's AI harness(es)).
```

- [ ] **Step 6: Write the dispatch test** — append to the `tests` module in `crates/mn-mcp/src/tools.rs` (or create one if absent):

```rust
#[cfg(test)]
mod install_skill_tests {
    use super::*;

    #[test]
    fn manifest_includes_install_search_skill() {
        assert!(list().tools.iter().any(|t| t.name == "install_search_skill"));
    }

    #[test]
    fn install_rejects_bad_scope() {
        let args = json!({ "scope": "global" });
        let err = run_install_search_skill(&args).unwrap_err();
        assert!(matches!(err.0, crate::protocol::ErrorCode::InvalidParams));
    }

    #[test]
    fn install_rejects_unknown_harness() {
        let args = json!({ "harness": ["windsurf"] });
        let err = run_install_search_skill(&args).unwrap_err();
        assert!(matches!(err.0, crate::protocol::ErrorCode::InvalidParams));
    }

    #[test]
    fn install_writes_into_injected_fake_home() {
        // No global env mutation: inject a fake SkillEnv pointing at a tempdir.
        struct FakeEnv {
            home: std::path::PathBuf,
        }
        impl mn_skills::SkillEnv for FakeEnv {
            fn home_dir(&self) -> Option<std::path::PathBuf> {
                Some(self.home.clone())
            }
            fn current_dir(&self) -> Option<std::path::PathBuf> {
                Some(self.home.clone())
            }
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env = FakeEnv { home: tmp.path().to_path_buf() };
        // Force `cursor` so the result doesn't depend on what's installed on the
        // machine.
        let text = run_install_search_skill_in(&json!({ "harness": ["cursor"], "scope": "user" }), &env)
            .expect("install ok");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["installed"][0]["harness"], "cursor");
        assert!(tmp.path().join(".cursor/skills/midnight-advanced-search/SKILL.md").exists());
    }
}
```

- [ ] **Step 7: Update the contract `mcp-tools.json`**

In `specs/001-rag-platform/contracts/mcp-tools.json`:
- Bump `"version": "1.1.0"` → `"version": "1.2.0"`.
- Add to the top-of-file `description` a note: ` PR #64 added install_search_skill (12 tools).` (append to the existing sentence).
- Add this entry inside `"tools"` after the `"status"` entry:

```json
    "install_search_skill": {
      "description": "Install the midnight-advanced-search Agent Skill into the user's AI harness(es). Writes the same SKILL.md to each detected harness's native skills directory; re-running updates in place. Returns, per harness, the scope, the exact path written, the action (created/updated/unchanged), and the reload step to relay to the user. Optional `harness` forces specific targets; omit to auto-detect. Optional `scope` is user (default) or project.",
      "input_schema": {
        "type": "object",
        "properties": {
          "harness": {
            "type": "array",
            "items": { "type": "string", "enum": ["claude-code", "codex", "opencode", "cursor"] }
          },
          "scope": { "type": "string", "enum": ["user", "project"], "default": "user" }
        },
        "additionalProperties": false
      },
      "output_schema": {
        "type": "object",
        "properties": {
          "skill_name": { "type": "string" },
          "scope": { "type": "string" },
          "installed": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "harness": { "type": "string" },
                "scope": { "type": "string" },
                "path": { "type": "string" },
                "action": { "type": "string", "enum": ["created", "updated", "unchanged"] },
                "reload_step": { "type": "string" }
              }
            }
          },
          "not_detected": { "type": "array", "items": { "type": "string" } }
        }
      }
    },
```

(Add a comma after the `"status"` entry's closing brace so the JSON stays valid.)

- [ ] **Step 8: Run tests**

Run: `cargo test -p mn-mcp && cargo test -p mn-telemetry`
Expected: PASS, including `every_manifest_tool_has_a_telemetry_name` (server.rs) and the new install tests.

- [ ] **Step 9: Validate the contract JSON parses**

Run: `cargo run -q -p mn-cli --bin mnm -- version >/dev/null && python3 -c "import json;json.load(open('specs/001-rag-platform/contracts/mcp-tools.json'))" 2>/dev/null || node -e "require('./specs/001-rag-platform/contracts/mcp-tools.json')" 2>/dev/null || rg -q '"install_search_skill"' specs/001-rag-platform/contracts/mcp-tools.json`

> The Python/Node calls are validators only and may be absent — the `rg` fallback confirms the entry exists. Do NOT install anything to validate JSON. Prefer eyeballing the diff for balanced braces/commas.

- [ ] **Step 10: Run the gate**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS, no warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/mn-mcp crates/mn-telemetry/src/events.rs specs/001-rag-platform/contracts/mcp-tools.json
git commit -m "feat(mn-mcp): install_search_skill tool + contract bump 1.2.0"
```

---

## Task 7: MCP `prompts` capability + `add_advanced_search_skill`

**Files:**
- Modify: `crates/mn-mcp/src/protocol.rs` (prompts wire types + capability)
- Create: `crates/mn-mcp/src/prompts.rs`
- Modify: `crates/mn-mcp/src/server.rs` (capability in `initialize`, `prompts/list`, `prompts/get`)
- Modify: `crates/mn-mcp/src/lib.rs` (`pub mod prompts;`)
- Create: `specs/001-rag-platform/contracts/mcp-prompts.json`

- [ ] **Step 1: Add prompts wire types to `protocol.rs`**

In `crates/mn-mcp/src/protocol.rs`, add `prompts` to `ServerCapabilities`:

```rust
/// Server capabilities advertised in the `initialize` response.
#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    /// Tool support.
    pub tools: ToolsCapability,
    /// Prompt support.
    pub prompts: PromptsCapability,
}

/// Prompt capability flags.
#[derive(Debug, Serialize)]
pub struct PromptsCapability {
    /// Whether the prompt list can change at runtime.
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}
```

Add the prompt types (place after `ToolsListResult`):

```rust
/// One declared argument of a prompt.
#[derive(Debug, Serialize)]
pub struct PromptArgument {
    /// Argument name.
    pub name: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Whether the client must supply it.
    pub required: bool,
}

/// One prompt declaration in a `prompts/list` response.
#[derive(Debug, Serialize)]
pub struct PromptDescription {
    /// Prompt name (e.g. `add_advanced_search_skill`).
    pub name: &'static str,
    /// Human-readable description (shown in client prompt menus).
    pub description: &'static str,
    /// Declared arguments.
    pub arguments: Vec<PromptArgument>,
}

/// `prompts/list` response payload.
#[derive(Debug, Serialize)]
pub struct PromptsListResult {
    /// All available prompts.
    pub prompts: Vec<PromptDescription>,
}

/// `prompts/get` request params.
#[derive(Debug, Deserialize)]
pub struct PromptGetParams {
    /// Prompt name to render.
    pub name: String,
    /// Caller-supplied arguments (string → string map per MCP).
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// One message in a rendered prompt.
#[derive(Debug, Serialize)]
pub struct PromptMessage {
    /// `"user"` or `"assistant"`.
    pub role: &'static str,
    /// Message content (a single text block).
    pub content: ContentBlock,
}

/// `prompts/get` response payload.
#[derive(Debug, Serialize)]
pub struct PromptGetResult {
    /// Human-readable description of the rendered prompt.
    pub description: String,
    /// The rendered messages.
    pub messages: Vec<PromptMessage>,
}
```

- [ ] **Step 2: Write `crates/mn-mcp/src/prompts.rs`**

```rust
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
    use mn_skills::{Harness, Scope};
    use std::str::FromStr as _;

    if params.name != ADD_SKILL_PROMPT {
        return Response::err(id, ErrorCode::InvalidParams, format!("unknown prompt: {}", params.name));
    }

    // Arguments arrive as a JSON object of string values (MCP prompt args).
    let arg = |key: &str| -> Option<String> {
        params.arguments.get(key).and_then(|v| v.as_str()).map(str::to_owned)
    };

    // Validate scope if present.
    let scope_arg = arg("scope");
    if let Some(s) = &scope_arg {
        if Scope::from_str(s).is_err() {
            return Response::err(id, ErrorCode::InvalidParams, format!("unknown scope `{s}`"));
        }
    }

    // Validate harness list if present.
    let harness_arg = arg("harness");
    if let Some(raw) = &harness_arg {
        for tok in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if Harness::from_str(tok).is_err() {
                return Response::err(id, ErrorCode::InvalidParams, format!("unknown harness `{tok}`"));
            }
        }
    }

    let tool_args = build_tool_args(harness_arg.as_deref(), scope_arg.as_deref());
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
fn build_tool_args(harness: Option<&str>, scope: Option<&str>) -> String {
    let mut obj = serde_json::Map::new();
    if let Some(raw) = harness {
        let list: Vec<&str> = raw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        obj.insert("harness".to_owned(), serde_json::json!(list));
    }
    if let Some(s) = scope {
        obj.insert("scope".to_owned(), serde_json::json!(s));
    }
    serde_json::Value::Object(obj).to_string()
}

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
    use crate::protocol::{PromptGetParams, RequestId};

    fn params(name: &str, args: serde_json::Value) -> PromptGetParams {
        PromptGetParams { name: name.to_owned(), arguments: args }
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
        let text = v["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("arguments: {}"));
        assert_eq!(v["result"]["messages"][0]["role"], "user");
    }

    #[test]
    fn get_embeds_supplied_args() {
        let r = get(
            RequestId::Number(1),
            &params(ADD_SKILL_PROMPT, serde_json::json!({ "harness": "cursor,codex", "scope": "project" })),
        );
        let v = serde_json::to_value(&r).unwrap();
        let text = v["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("\"harness\":[\"cursor\",\"codex\"]"));
        assert!(text.contains("\"scope\":\"project\""));
    }

    #[test]
    fn get_rejects_bad_scope_and_harness() {
        let bad_scope = get(RequestId::Number(1), &params(ADD_SKILL_PROMPT, serde_json::json!({ "scope": "global" })));
        assert!(bad_scope.error.is_some());
        let bad_h = get(RequestId::Number(1), &params(ADD_SKILL_PROMPT, serde_json::json!({ "harness": "windsurf" })));
        assert!(bad_h.error.is_some());
    }
}
```

- [ ] **Step 3: Wire the module + capability + handlers in `server.rs` and `lib.rs`**

In `crates/mn-mcp/src/lib.rs`, add `pub mod prompts;` with the other `pub mod` lines.

In `crates/mn-mcp/src/server.rs`, update the `initialize` arm's `ServerCapabilities` to include prompts:

```rust
                capabilities: ServerCapabilities {
                    tools: ToolsCapability { list_changed: false },
                    prompts: crate::protocol::PromptsCapability { list_changed: false },
                },
```

Add the two method arms in `handle_request`'s `match req.method.as_str()`, after the `"tools/call"` arm:

```rust
        "prompts/list" => Response::success(
            id.clone(),
            serde_json::to_value(crate::prompts::list()).expect("serialize prompt list"),
        ),
        "prompts/get" => match serde_json::from_value::<crate::protocol::PromptGetParams>(
            req.params.clone(),
        ) {
            Ok(params) => crate::prompts::get(id.clone(), &params),
            Err(e) => Response::err(id.clone(), ErrorCode::InvalidParams, e.to_string()),
        },
```

Ensure `ServerCapabilities` import already exists (it does). No new imports needed beyond what's used inline (fully-qualified).

- [ ] **Step 4: Add an integration-style server test** — append to the `tests` module in `crates/mn-mcp/src/server.rs`:

```rust
    /// `initialize` must advertise the prompts capability so clients query it.
    #[test]
    fn initialize_advertises_prompts_capability() {
        let init = InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION,
            capabilities: ServerCapabilities {
                tools: ToolsCapability { list_changed: false },
                prompts: crate::protocol::PromptsCapability { list_changed: false },
            },
            server_info: ServerInfo { name: "x", version: "0" },
        };
        let v = serde_json::to_value(&init).unwrap();
        assert_eq!(v["capabilities"]["prompts"]["listChanged"], false);
    }
```

- [ ] **Step 5: Write the prompts contract** — create `specs/001-rag-platform/contracts/mcp-prompts.json`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "midnight-manual MCP prompts",
  "description": "MCP prompts exposed by `mnm mcp serve`. Implemented against the MCP prompts spec (2025-06-18). The runtime manifest lives at `crates/mn-mcp/src/prompts.rs::list()`; this document mirrors it.",
  "version": "1.0.0",
  "capability": { "prompts": { "listChanged": false } },
  "prompts": {
    "add_advanced_search_skill": {
      "description": "Install the midnight-advanced-search skill into this session's AI harness so the assistant uses the advanced retrieval playbook automatically. Checks whether it's already present and installs it if not, then tells the user how to reload. Surfaced to users as /mnm:add-advanced-search-skill (clients may namespace, e.g. /mcp__midnight-manual__add_advanced_search_skill).",
      "arguments": [
        { "name": "harness", "description": "Optional comma-separated harnesses (claude-code, codex, opencode, cursor). Omit to auto-detect.", "required": false },
        { "name": "scope", "description": "Optional install scope: user (default) or project.", "required": false }
      ],
      "result": {
        "description": "string",
        "messages": [
          { "role": "user", "content": { "type": "text", "text": "Instruction to call install_search_skill (forwarding harness/scope when supplied) and relay the per-harness reload step." } }
        ]
      }
    }
  }
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p mn-mcp`
Expected: PASS, including `prompts::tests` and `initialize_advertises_prompts_capability`.

- [ ] **Step 7: End-to-end JSON-RPC smoke test over stdio**

Run:
```bash
printf '%s\n%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"prompts/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"prompts/get","params":{"name":"add_advanced_search_skill"}}' \
  | cargo run -q -p mn-cli --bin mnm -- mcp serve 2>/dev/null
```
Expected: three JSON-RPC responses; id 1 shows `"prompts":{"listChanged":false}`; id 2 lists `add_advanced_search_skill`; id 3 returns a `messages` array with a user-role text block mentioning `install_search_skill`.

> If `mcp serve` blocks waiting for more input, that's fine — pipe closes on EOF and the server exits. If it needs a `notifications/initialized`, the three responses still emit first.

- [ ] **Step 8: Run the gate**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS, no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/mn-mcp specs/001-rag-platform/contracts/mcp-prompts.json
git commit -m "feat(mn-mcp): prompts capability + add_advanced_search_skill prompt"
```

---

## Task 8: README Cursor row + cross-doc consistency

**Files:**
- Modify: `README.md` (Cursor row + "adapted rule" wording)
- Modify: `crates/mn-mcp/Cargo.toml` (package description "seven" → "twelve")

- [ ] **Step 1: Update the README "Supported harnesses" table**

In `README.md`, in the "Supported harnesses" table (around line 236-241), replace the Cursor row and the lead-in sentence.

Replace the lead-in:
```markdown
The skill ships in each harness's native format — the same portable `SKILL.md` everywhere it's supported, and an adapted rule where it isn't:
```
with:
```markdown
The skill ships as the same portable `SKILL.md` in every supported harness:
```

Replace the Cursor row:
```markdown
| **Cursor** | Project Rule (`.mdc`) | `.cursor/rules/` |
```
with:
```markdown
| **Cursor** | `SKILL.md` (Agent Skill, Cursor 2.4+) | `~/.cursor/skills/` · `.cursor/skills/` |
```

- [ ] **Step 2: Verify no other README claim now contradicts the design**

Run: `rg -n "\.mdc|Project Rule|adapted rule" README.md`
Expected: no matches. If any remain in the Advanced-search section, reword to `SKILL.md`. (Matches elsewhere unrelated to skills are fine — judge by context.)

- [ ] **Step 3: Update the MCP crate description**

In `crates/mn-mcp/Cargo.toml`, change:
```toml
description = "MCP JSON-RPC server over stdio with seven retrieval tools."
```
to:
```toml
description = "MCP JSON-RPC server over stdio: twelve tools plus the add_advanced_search_skill prompt."
```

- [ ] **Step 4: Confirm the SKILL.md ↔ cookbook ↔ tool-description DRY chain**

Run: `rg -n "query-enhancement.md" crates/mn-skills/assets crates/mn-mcp/src/tools.rs docs/cookbook`
Expected: the SKILL.md links the cookbook; the `search` tool description already references it; the cookbook exists. No content duplication beyond the short technique summaries. (No code change unless a reference is missing.)

- [ ] **Step 5: Run the gate**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add README.md crates/mn-mcp/Cargo.toml
git commit -m "docs: README Cursor row -> SKILL.md; mn-mcp description"
```

---

## Final verification (after all tasks)

- [ ] **Full workspace gate**

Run: `just check` (or the three gate commands).
Expected: PASS across the whole workspace.

- [ ] **MSRV gate**

Run: `just check-msrv`
Expected: PASS against pinned 1.91.

- [ ] **End-to-end manual confirmation**

Run:
```bash
TMP=$(mktemp -d); mkdir -p "$TMP/.claude" "$TMP/.cursor"
HOME="$TMP" cargo run -q -p mn-cli --bin mnm -- skills add
HOME="$TMP" cargo run -q -p mn-cli --bin mnm -- skills status
HOME="$TMP" cargo run -q -p mn-cli --bin mnm -- skills remove
```
Expected: `add` installs into claude-code + cursor (the two markers present), `status` shows them up-to-date, `remove` deletes them.

- [ ] **Stop. Do NOT push or open a PR.** Report to the human: summary of what shipped, the gate output, and ask whether to push + open a PR (and which base — stack on PR #63 / `docs/promo-readme`, or rebase onto `main` after #63 merges).

---

## Self-review notes (author)

- **Spec coverage:** crate (T1–T4), SKILL.md + DRY (T1, T8), CLI add/status/remove (T5), MCP tool writes-directly + contract bump (T6), prompts capability + prompt with harness/scope args (T7), README Cursor row (T8), telemetry arms (T5/T6), testing all tempfile/fake-home (every task). All spec sections map to a task.
- **Type consistency:** `Harness`/`Scope`/`SkillEnv`/`StdSkillEnv` defined T1–T2; `SkillError` T3 (`error.rs`); report types + `install`/`remove`/`status` T4; all consumers (T5–T7) use those exact names. `run_install_search_skill` returns `Result<String, (ErrorCode, String)>` — consumed in T6 server arm. `prompts::get` returns `Response` — consumed in T7 server arm.
- **No placeholders:** every code step shows complete code; commands have expected output.
- **Env injection:** `run_install_search_skill` delegates to `run_install_search_skill_in(args, &impl SkillEnv)` so the MCP tool test (T6) injects a fake home instead of mutating global `HOME` — no cross-test races.
- **`error.rs`:** defined in Task 3, shared by `detect` (T3) and `install` (T4).
