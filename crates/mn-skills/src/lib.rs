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
        let n = fm.description.chars().count();
        assert!(n <= 1024, "description must be <= 1024 chars (open-standard cap); was {n}");
    }

    #[test]
    fn name_matches_open_standard_regex() {
        // ^[a-z0-9]+(-[a-z0-9]+)*$ — lowercase alnum, single-hyphen separated.
        let ok = SKILL_NAME.split('-').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
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
