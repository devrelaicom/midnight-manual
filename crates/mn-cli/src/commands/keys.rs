//! `mnm keys generate` — mint an Ed25519 keypair for admin auth (FR-067).
//!
//! Behavior:
//!
//! 1. Generate a fresh Ed25519 keypair using the OS RNG.
//!
//! 2. Write the 32-byte signing seed to
//!    `$XDG_CONFIG_HOME/midnight-manual/keys/<user_id>.private` with mode
//!    `0o600` on Unix.
//!
//! 3. Echo the public half to stdout in the canonical
//!    `ed25519:<base64>` wire form so the operator can paste it into
//!    `users.toml`.
//!
//! The private half is NEVER echoed to stdout / stderr / logs.

use anyhow::{anyhow, Context as _, Result};
use clap::{Args as ClapArgs, Subcommand};
use mn_auth::Keypair;
use serde::Serialize;

/// `mnm keys <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: KeysCmd,
}

/// `keys` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum KeysCmd {
    /// Generate a new keypair, persist the private half locally, print the
    /// public half in `users.toml` wire form.
    Generate(GenerateArgs),
}

/// Args for `mnm keys generate`.
#[derive(Debug, ClapArgs)]
pub struct GenerateArgs {
    /// User id the keypair is bound to. Becomes the `user_id` field in the
    /// echoed TOML row and the basename of the private-key file.
    #[arg(long)]
    pub user_id: String,

    /// Print the public half + intended write path without touching the
    /// filesystem.
    #[arg(long)]
    pub dry_run: bool,

    /// Overwrite an existing `<user_id>.private` file. Default is to refuse —
    /// we never silently rotate a key out from under a user.
    #[arg(long)]
    pub force: bool,
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error when the keys directory cannot be resolved, when the
/// private-key file already exists and `--force` was not supplied, or when
/// the filesystem write itself fails.
pub fn run(args: Args, json: bool) -> Result<()> {
    match args.cmd {
        KeysCmd::Generate(a) => generate(&a, json),
    }
}

#[derive(Debug, Serialize)]
struct GenerateOutput<'a> {
    action: &'a str,
    user_id: &'a str,
    public_key: &'a str,
    private_key_path: String,
    toml_row: String,
    dry_run: bool,
}

fn generate(args: &GenerateArgs, json: bool) -> Result<()> {
    if args.user_id.trim().is_empty() {
        return Err(anyhow!("--user-id must be non-empty"));
    }

    let env = mn_core::config::StdEnv;
    let private_path = mn_core::paths::private_key_path(&env, &args.user_id).ok_or_else(|| {
        anyhow!(
            "could not resolve key storage dir (set XDG_CONFIG_HOME or HOME so we know where to write `<user_id>.private`)"
        )
    })?;

    let kp = Keypair::generate();
    let public_wire = kp.public_wire();
    let toml_row = format_toml_row(&args.user_id, &public_wire);

    if !args.dry_run {
        if private_path.exists() && !args.force {
            return Err(anyhow!(
                "private key already exists at `{}`; pass --force to overwrite",
                private_path.display(),
            ));
        }
        if let Some(parent) = private_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create keys dir {}", parent.display()))?;
        }
        write_private_key(&private_path, &kp.signing_bytes())?;
    }

    let out = GenerateOutput {
        action: "keys.generate",
        user_id: &args.user_id,
        public_key: &public_wire,
        private_key_path: private_path.display().to_string(),
        toml_row: toml_row.clone(),
        dry_run: args.dry_run,
    };

    if json {
        let body = serde_json::to_string(&out).context("serialize json output")?;
        println!("{body}");
    } else {
        if args.dry_run {
            println!("# DRY RUN — nothing written.");
        } else {
            println!("# wrote private key to {} (mode 0o600)", private_path.display(),);
        }
        println!("# paste the following row into your user-store TOML:");
        println!("{toml_row}");
    }

    Ok(())
}

/// Render a ready-to-paste `[[users]]` TOML row. The `created_at` field is
/// stamped to today's UTC date so the row matches the user-store schema.
fn format_toml_row(user_id: &str, public_wire: &str) -> String {
    use std::fmt::Write as _;
    let today = time::OffsetDateTime::now_utc().date();
    // Manual YYYY-MM-DD render — `time::Date::Display` formats as a different
    // shape (`2026-05-14` is the same here but the trait is stability-fragile,
    // so we spell it out and stay independent of trait impl drift).
    let date = format!("{:04}-{:02}-{:02}", today.year(), u8::from(today.month()), today.day(),);
    let mut s = String::new();
    s.push_str("[[users]]\n");
    let _ = writeln!(s, "user_id    = \"{user_id}\"");
    s.push_str("role       = \"admin\"\n");
    let _ = writeln!(s, "public_key = \"{public_wire}\"");
    let _ = writeln!(s, "created_at = \"{date}\"");
    s
}

#[cfg(unix)]
fn write_private_key(path: &std::path::Path, seed: &[u8; 32]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("private.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(seed)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.flush()
            .with_context(|| format!("flush {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_key(path: &std::path::Path, seed: &[u8; 32]) -> Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension("private.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(seed)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.flush()
            .with_context(|| format!("flush {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Load a 32-byte Ed25519 signing seed from disk, enforcing `0o600`.
///
/// # Errors
///
/// Returns an error when the file is missing, has loose permissions
/// (any group / world bits set on Unix), or is not exactly 32 bytes.
pub fn load_private_key(path: &std::path::Path) -> Result<[u8; 32]> {
    let md = std::fs::metadata(path)
        .with_context(|| format!("stat private key `{}`", path.display()))?;
    check_private_key_perms(path, &md)?;
    let body =
        std::fs::read(path).with_context(|| format!("read private key `{}`", path.display()))?;
    if body.len() != 32 {
        return Err(anyhow!(
            "private key at `{}` has {} bytes (expected 32)",
            path.display(),
            body.len(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&body);
    Ok(seed)
}

#[cfg(unix)]
fn check_private_key_perms(path: &std::path::Path, md: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = md.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(anyhow!(
            "private key `{}` has insecure permissions ({mode:#o}); expected 0o600. Run `chmod 600 \"{}\"` and retry.",
            path.display(),
            path.display(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private_key_perms(_path: &std::path::Path, _md: &std::fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_user_id_rejected() {
        let err = generate(
            &GenerateArgs {
                user_id: String::new(),
                dry_run: true,
                force: false,
            },
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("user-id"));
    }

    #[test]
    fn toml_row_includes_required_fields() {
        let row = format_toml_row("aaron", "ed25519:AAAA");
        assert!(row.contains("[[users]]"));
        assert!(row.contains("user_id    = \"aaron\""));
        assert!(row.contains("role       = \"admin\""));
        assert!(row.contains("public_key = \"ed25519:AAAA\""));
        assert!(row.contains("created_at = "));
    }

    #[cfg(unix)]
    #[test]
    fn write_and_load_round_trips_with_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.private");
        let seed = [7u8; 32];
        write_private_key(&path, &seed).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let loaded = load_private_key(&path).unwrap();
        assert_eq!(loaded, seed);
    }

    #[cfg(unix)]
    #[test]
    fn load_rejects_world_readable_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.private");
        std::fs::write(&path, [0u8; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = load_private_key(&path).unwrap_err();
        assert!(err.to_string().contains("insecure"));
    }

    #[test]
    fn load_rejects_wrong_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.private");
        std::fs::write(&path, [0u8; 16]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let err = load_private_key(&path).unwrap_err();
        assert!(err.to_string().contains("16 bytes"));
    }
}
