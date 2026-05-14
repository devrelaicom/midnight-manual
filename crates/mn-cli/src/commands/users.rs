//! `mnm users {list|show|add|update|remove}` — local user-store CRUD
//! (FR-070, FR-071).
//!
//! The user-store TOML is the authority for `user_id → public_key + role`
//! lookups at the server. The CLI mutates the local file; the deploy step
//! is the operator's job. Every mutation prints a deploy-warning so a quick
//! `mnm users add ...` doesn't lull the operator into thinking the change
//! is live.
//!
//! Path precedence: `MIDNIGHT_MANUAL_USER_STORE` env var > XDG-derived
//! `<config_home>/users.toml`. (Note: on the **server** boot path the same
//! env var is treated as the TOML body, not a path — see
//! `crates/mn-core/src/paths.rs` and `crates/mn-server/src/config.rs`.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use mn_auth::{parse_public_key_wire, Role};
use serde::{Deserialize, Serialize};

/// `mnm users <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: UsersCmd,
}

/// `users` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum UsersCmd {
    /// List users in the local user-store.
    List,
    /// Show one user by id.
    Show {
        /// User id.
        user_id: String,
    },
    /// Add a new user.
    Add(AddArgs),
    /// Update an existing user's role / public_key / note.
    Update(UpdateArgs),
    /// Remove a user from the local store.
    Remove(RemoveArgs),
}

/// Role choices on the CLI side. Mirrors [`mn_auth::Role`] but stays a
/// stand-alone enum so `clap::ValueEnum` can derive without churning the
/// shared role type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliRole {
    /// Admin tier — full surface.
    Admin,
    /// Writer tier — ingest-only.
    Writer,
}

impl From<CliRole> for Role {
    fn from(value: CliRole) -> Self {
        match value {
            CliRole::Admin => Self::Admin,
            CliRole::Writer => Self::Writer,
        }
    }
}

/// Args for `mnm users add`.
#[derive(Debug, ClapArgs)]
pub struct AddArgs {
    /// New user's id (the JWT `sub` claim).
    #[arg(long)]
    pub user_id: String,
    /// New user's role.
    #[arg(long, value_enum)]
    pub role: CliRole,
    /// Public key in `ed25519:<base64>` wire form (the line `mnm keys
    /// generate` echoes to stdout).
    #[arg(long)]
    pub public_key: String,
    /// Optional human note.
    #[arg(long)]
    pub note: Option<String>,
    /// Validate inputs without writing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Args for `mnm users update`.
#[derive(Debug, ClapArgs)]
pub struct UpdateArgs {
    /// User id to update.
    #[arg(long)]
    pub user_id: String,
    /// New role (unchanged if omitted).
    #[arg(long, value_enum)]
    pub role: Option<CliRole>,
    /// New public key in `ed25519:<base64>` wire form (unchanged if omitted).
    #[arg(long)]
    pub public_key: Option<String>,
    /// New note. Pass `--note ""` to clear an existing note.
    #[arg(long)]
    pub note: Option<String>,
    /// Validate inputs without writing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Args for `mnm users remove`.
#[derive(Debug, ClapArgs)]
pub struct RemoveArgs {
    /// User id to remove.
    #[arg(long)]
    pub user_id: String,
    /// Validate inputs without writing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error when the user-store path cannot be resolved, when the
/// TOML cannot be parsed, when validation fails (missing user, duplicate
/// id, malformed public key), or when the on-disk write fails.
pub fn run(args: Args, json: bool) -> Result<()> {
    let path = resolve_user_store_path()?;
    match args.cmd {
        UsersCmd::List => list(&path, json),
        UsersCmd::Show { user_id } => show(&path, &user_id, json),
        UsersCmd::Add(a) => add(&path, &a, json),
        UsersCmd::Update(a) => update(&path, &a, json),
        UsersCmd::Remove(a) => remove(&path, &a, json),
    }
}

fn resolve_user_store_path() -> Result<PathBuf> {
    mn_core::paths::user_store_path(&mn_core::config::StdEnv).ok_or_else(|| {
        anyhow!(
            "could not resolve user-store path (set MIDNIGHT_MANUAL_USER_STORE, XDG_CONFIG_HOME, or HOME)"
        )
    })
}

/// On-disk TOML row. Mirrors [`mn_auth::User`] but stays a CLI-local type so
/// we can serialize without dragging the `deny_unknown_fields` parser config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserRow {
    user_id: String,
    role: String,
    public_key: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// On-disk file shape.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreFile {
    schema_version: u32,
    #[serde(default)]
    users: Vec<UserRow>,
}

impl StoreFile {
    fn read(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                schema_version: mn_auth::USER_STORE_SCHEMA_VERSION,
                users: Vec::new(),
            });
        }
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read user store {}", path.display()))?;
        let file: Self = toml::from_str(&body)
            .with_context(|| format!("parse user store {}", path.display()))?;
        if file.schema_version > mn_auth::USER_STORE_SCHEMA_VERSION {
            return Err(anyhow!(
                "user store schema_version={} is newer than supported (max {})",
                file.schema_version,
                mn_auth::USER_STORE_SCHEMA_VERSION,
            ));
        }
        Ok(file)
    }

    fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create parent {}", parent.display()))?;
            }
        }
        let body = toml::to_string(self).context("serialize user store")?;
        // tmp-then-rename so concurrent readers see one full file or the
        // other; never a half-written intermediate.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }
}

fn list(path: &Path, json: bool) -> Result<()> {
    let file = StoreFile::read(path)?;
    if json {
        emit_json("users.list", path, false, |o| {
            o.insert("users".into(), serde_json::to_value(&file.users)?);
            Ok(())
        })?;
    } else if file.users.is_empty() {
        println!("# {} (empty)", path.display());
    } else {
        println!("# {}", path.display());
        for u in &file.users {
            let note = u.note.as_deref().unwrap_or("");
            println!("{:<24} {:<7} {} {}", u.user_id, u.role, u.public_key, note);
        }
    }
    Ok(())
}

fn show(path: &Path, user_id: &str, json: bool) -> Result<()> {
    let file = StoreFile::read(path)?;
    let row = file
        .users
        .iter()
        .find(|u| u.user_id == user_id)
        .ok_or_else(|| anyhow!("user `{user_id}` not found in {}", path.display()))?;
    if json {
        emit_json("users.show", path, false, |o| {
            o.insert("user".into(), serde_json::to_value(row)?);
            Ok(())
        })?;
    } else {
        println!("user_id:    {}", row.user_id);
        println!("role:       {}", row.role);
        println!("public_key: {}", row.public_key);
        println!("created_at: {}", row.created_at);
        if let Some(n) = &row.note {
            println!("note:       {n}");
        }
    }
    Ok(())
}

fn add(path: &Path, args: &AddArgs, json: bool) -> Result<()> {
    if args.user_id.trim().is_empty() {
        return Err(anyhow!("--user-id must be non-empty"));
    }
    validate_public_key(&args.public_key)?;

    let mut file = StoreFile::read(path)?;
    if file.users.iter().any(|u| u.user_id == args.user_id) {
        return Err(anyhow!("user `{}` already exists in {}", args.user_id, path.display()));
    }
    let role: Role = args.role.into();
    let row = UserRow {
        user_id: args.user_id.clone(),
        role: role.as_wire().to_owned(),
        public_key: args.public_key.clone(),
        created_at: today_iso_date(),
        note: args.note.clone(),
    };

    if !args.dry_run {
        file.users.push(row.clone());
        file.write(path)?;
    }
    emit_mutation("users.add", path, args.dry_run, json, |o| {
        o.insert("user".into(), serde_json::to_value(&row)?);
        Ok(())
    })
}

fn update(path: &Path, args: &UpdateArgs, json: bool) -> Result<()> {
    if let Some(pk) = &args.public_key {
        validate_public_key(pk)?;
    }
    let mut file = StoreFile::read(path)?;
    let idx = file
        .users
        .iter()
        .position(|u| u.user_id == args.user_id)
        .ok_or_else(|| anyhow!("user `{}` not found in {}", args.user_id, path.display()))?;

    let updated = {
        let row = &mut file.users[idx];
        if let Some(role) = args.role {
            let role: Role = role.into();
            role.as_wire().clone_into(&mut row.role);
        }
        if let Some(pk) = &args.public_key {
            row.public_key.clone_from(pk);
        }
        if let Some(note) = &args.note {
            row.note = if note.is_empty() {
                None
            } else {
                Some(note.clone())
            };
        }
        row.clone()
    };

    if !args.dry_run {
        file.write(path)?;
    }
    emit_mutation("users.update", path, args.dry_run, json, |o| {
        o.insert("user".into(), serde_json::to_value(&updated)?);
        Ok(())
    })
}

fn remove(path: &Path, args: &RemoveArgs, json: bool) -> Result<()> {
    let mut file = StoreFile::read(path)?;
    let before = file.users.len();
    file.users.retain(|u| u.user_id != args.user_id);
    if file.users.len() == before {
        return Err(anyhow!("user `{}` not found in {}", args.user_id, path.display(),));
    }
    if !args.dry_run {
        file.write(path)?;
    }
    emit_mutation("users.remove", path, args.dry_run, json, |o| {
        o.insert("user_id".into(), serde_json::Value::String(args.user_id.clone()));
        Ok(())
    })
}

fn validate_public_key(pk: &str) -> Result<()> {
    parse_public_key_wire(pk).map_err(|e| anyhow!("invalid public_key (`{pk}`): {e}"))?;
    Ok(())
}

fn today_iso_date() -> String {
    let today = time::OffsetDateTime::now_utc().date();
    format!("{:04}-{:02}-{:02}", today.year(), u8::from(today.month()), today.day(),)
}

/// Print a deploy-warning + JSON-or-human payload after a user-store mutation.
fn emit_mutation<F>(action: &str, path: &Path, dry_run: bool, json: bool, build: F) -> Result<()>
where
    F: FnOnce(&mut BTreeMap<String, serde_json::Value>) -> Result<()>,
{
    emit_json_or_human(action, path, dry_run, json, build)?;
    if json {
        eprintln!("# user-store change is LOCAL ONLY — deploy the updated file for it to take effect (FR-071)");
    } else {
        let kind = if dry_run { "(DRY RUN) " } else { "" };
        eprintln!(
            "{kind}user-store change is LOCAL ONLY: deploy `{}` for it to take effect (FR-071)",
            path.display(),
        );
    }
    Ok(())
}

fn emit_json<F>(action: &str, path: &Path, dry_run: bool, build: F) -> Result<()>
where
    F: FnOnce(&mut BTreeMap<String, serde_json::Value>) -> Result<()>,
{
    emit_json_or_human(action, path, dry_run, true, build)
}

fn emit_json_or_human<F>(
    action: &str,
    path: &Path,
    dry_run: bool,
    json: bool,
    build: F,
) -> Result<()>
where
    F: FnOnce(&mut BTreeMap<String, serde_json::Value>) -> Result<()>,
{
    let mut o: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    o.insert("action".into(), serde_json::Value::String(action.into()));
    o.insert("user_store".into(), serde_json::Value::String(path.display().to_string()));
    o.insert("dry_run".into(), serde_json::Value::Bool(dry_run));
    build(&mut o)?;
    if json {
        let body = serde_json::to_string(&o).context("serialize json output")?;
        println!("{body}");
    } else {
        match action {
            "users.add" => println!("added user (run `mnm users show <id>` to confirm)"),
            "users.update" => println!("updated user"),
            "users.remove" => println!("removed user"),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_store(dir: &Path) -> PathBuf {
        dir.join("users.toml")
    }

    fn fixture_public_key() -> String {
        mn_auth::Keypair::generate().public_wire()
    }

    #[test]
    fn add_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let pk = fixture_public_key();
        add(
            &path,
            &AddArgs {
                user_id: "aaron".into(),
                role: CliRole::Admin,
                public_key: pk.clone(),
                note: Some("founding admin".into()),
                dry_run: false,
            },
            true,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("aaron"));
        assert!(body.contains(&pk));
        assert!(body.contains("founding admin"));
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let pk = fixture_public_key();
        add(
            &path,
            &AddArgs {
                user_id: "ghost".into(),
                role: CliRole::Writer,
                public_key: pk,
                note: None,
                dry_run: true,
            },
            true,
        )
        .unwrap();
        assert!(!path.exists(), "--dry-run must not write");
    }

    #[test]
    fn add_rejects_duplicate_user_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let pk = fixture_public_key();
        add(
            &path,
            &AddArgs {
                user_id: "aaron".into(),
                role: CliRole::Admin,
                public_key: pk.clone(),
                note: None,
                dry_run: false,
            },
            true,
        )
        .unwrap();
        let err = add(
            &path,
            &AddArgs {
                user_id: "aaron".into(),
                role: CliRole::Writer,
                public_key: pk,
                note: None,
                dry_run: false,
            },
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn add_rejects_bad_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let err = add(
            &path,
            &AddArgs {
                user_id: "aaron".into(),
                role: CliRole::Admin,
                public_key: "rsa:nope".into(),
                note: None,
                dry_run: true,
            },
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid public_key"));
    }

    #[test]
    fn update_changes_role_and_keeps_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let pk = fixture_public_key();
        add(
            &path,
            &AddArgs {
                user_id: "aaron".into(),
                role: CliRole::Writer,
                public_key: pk.clone(),
                note: None,
                dry_run: false,
            },
            true,
        )
        .unwrap();
        update(
            &path,
            &UpdateArgs {
                user_id: "aaron".into(),
                role: Some(CliRole::Admin),
                public_key: None,
                note: None,
                dry_run: false,
            },
            true,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("role = \"admin\""), "body was {body}");
        assert!(body.contains(&pk), "public key must be preserved");
    }

    #[test]
    fn update_clears_note_with_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let pk = fixture_public_key();
        add(
            &path,
            &AddArgs {
                user_id: "aaron".into(),
                role: CliRole::Admin,
                public_key: pk,
                note: Some("starter note".into()),
                dry_run: false,
            },
            true,
        )
        .unwrap();
        update(
            &path,
            &UpdateArgs {
                user_id: "aaron".into(),
                role: None,
                public_key: None,
                note: Some(String::new()),
                dry_run: false,
            },
            true,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("starter note"));
    }

    #[test]
    fn remove_unknown_user_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let err = remove(
            &path,
            &RemoveArgs {
                user_id: "ghost".into(),
                dry_run: false,
            },
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn remove_drops_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let pk = fixture_public_key();
        add(
            &path,
            &AddArgs {
                user_id: "aaron".into(),
                role: CliRole::Admin,
                public_key: pk,
                note: None,
                dry_run: false,
            },
            true,
        )
        .unwrap();
        remove(
            &path,
            &RemoveArgs {
                user_id: "aaron".into(),
                dry_run: false,
            },
            true,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("aaron"));
    }

    #[test]
    fn write_then_read_round_trips_via_mn_auth_loader() {
        // Round-trip: ensure the file we write parses cleanly with the
        // server-side strict loader. Catches drift between our writer's
        // shape and `mn_auth::UserStore::parse`.
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        let pk = fixture_public_key();
        add(
            &path,
            &AddArgs {
                user_id: "aaron".into(),
                role: CliRole::Admin,
                public_key: pk,
                note: Some("hello".into()),
                dry_run: false,
            },
            true,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let store = mn_auth::UserStore::parse(&body).expect("strict parse succeeds");
        assert_eq!(store.len(), 1);
        let u = store.get("aaron").unwrap();
        assert_eq!(u.role, Role::Admin);
        assert_eq!(u.note.as_deref(), Some("hello"));
    }

    #[test]
    fn list_empty_store_does_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_store(dir.path());
        list(&path, true).unwrap();
    }
}
