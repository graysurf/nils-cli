//! Command implementations: thin orchestration over `sops` and `git`.
//!
//! No-secret-leak contract (enforced here and exercised by the integration
//! tests):
//!
//! - `pull` decrypts by redirecting `sops -d` straight into the `./.env` file
//!   (mode `600`). The plaintext is never captured into a buffer that could be
//!   printed, and the success/JSON output carries only the store-relative path,
//!   the destination path, and the count of keys written — never any value.
//! - `add` asks `sops` to encrypt the source into a private temporary output
//!   under the store's `.git` directory, validates the ciphertext, then
//!   atomically renames it over the tracked target. The target therefore never
//!   contains plaintext, and failures leave the prior ciphertext untouched.
//! - `which` / `list` are pure metadata.
//! - `edit` execs `sops <file>` interactively; we inherit stdio so the editor
//!   round-trip stays inside sops and never passes through us.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::cli::BINARY;
use crate::store::{self, StoreEntry};

/// Schema version major for every `secrets` JSON envelope.
const SCHEMA_VERSION: u32 = 1;

/// Resolved, side-effect-free view of the environment a command runs against.
/// Injected explicitly so tests stay hermetic.
pub struct Env {
    /// Absolute path to the SOPS store checkout.
    pub store_root: PathBuf,
    /// The directory the command was invoked from (the app repo, for slug + .env).
    pub cwd: PathBuf,
}

impl Env {
    /// Resolve the real environment from `$SECRETS_REPO`, `$HOME`, and the CWD.
    fn from_process() -> Result<Self, CmdError> {
        let secrets_repo = std::env::var("SECRETS_REPO").ok();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let store_root = store::resolve_store_root(secrets_repo.as_deref(), home.as_deref())
            .ok_or_else(|| {
                CmdError::unavailable("store not found (set SECRETS_REPO)", "store-not-found")
            })?;
        let cwd = std::env::current_dir()
            .map_err(|err| CmdError::runtime(format!("cannot resolve current directory: {err}")))?;
        Ok(Self { store_root, cwd })
    }

    fn ensure_store(&self) -> Result<(), CmdError> {
        if self.store_root.join(".git").exists() {
            Ok(())
        } else {
            Err(CmdError::unavailable(
                format!(
                    "store not found at {} (set SECRETS_REPO)",
                    self.store_root.display()
                ),
                "store-not-found",
            ))
        }
    }
}

/// A structured command failure mapped to a stable exit code + error code.
struct CmdError {
    code: i32,
    error_code: String,
    message: String,
}

impl CmdError {
    fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: exit::RUNTIME,
            error_code: "runtime-error".to_string(),
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>, error_code: impl Into<String>) -> Self {
        Self {
            code: exit::UNAVAILABLE,
            error_code: error_code.into(),
            message: message.into(),
        }
    }

    fn no_entry(message: impl Into<String>) -> Self {
        Self {
            code: exit::DATA,
            error_code: "no-store-entry".to_string(),
            message: message.into(),
        }
    }
}

// ----- public command entrypoints (resolve real env, delegate, then emit) ----

pub fn pull(name: Option<&str>, format: OutputFormat) -> i32 {
    dispatch(format, |env| pull_with(env, name))
}

pub fn add(file: &str, format: OutputFormat) -> i32 {
    dispatch(format, |env| add_with(env, file))
}

pub fn list(format: OutputFormat) -> i32 {
    dispatch(format, list_with)
}

pub fn which(name: Option<&str>, format: OutputFormat) -> i32 {
    dispatch(format, |env| which_with(env, name))
}

pub fn edit(name: Option<&str>, format: OutputFormat) -> i32 {
    // `edit` execs an interactive editor through sops; it has no JSON payload.
    match Env::from_process().and_then(|env| edit_with(&env, name)) {
        Ok(code) => code,
        Err(err) => emit_error(format, &err),
    }
}

fn dispatch<T, F>(format: OutputFormat, run: F) -> i32
where
    T: Outcome,
    F: FnOnce(&Env) -> Result<T, CmdError>,
{
    match Env::from_process().and_then(|env| run(&env)) {
        Ok(outcome) => emit_success(format, outcome),
        Err(err) => emit_error(format, &err),
    }
}

// ----------------------------- command bodies --------------------------------

/// Resolve the store entry for an optional `[name]`, mirroring the bash lookup.
fn resolve_entry(env: &Env, name: Option<&str>) -> Result<StoreEntry, CmdError> {
    match name {
        Some(name) => Ok(store::store_entry_for_name(&env.store_root, name)),
        None => {
            let slug = repo_slug(env)?;
            Ok(store::store_entry_for_slug(&env.store_root, &slug))
        }
    }
}

/// Derive the `owner/repo` slug from the CWD repo's `origin` remote.
fn repo_slug(env: &Env) -> Result<String, CmdError> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(&env.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| CmdError::runtime(format!("failed to run git: {err}")))?;
    if !output.status.success() {
        return Err(CmdError::no_entry(
            "not in a git repo with an 'origin' remote — pass an explicit <name>",
        ));
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    store::slug_from_remote_url(&url)
        .ok_or_else(|| CmdError::no_entry(format!("could not derive a store slug from '{url}'")))
}

fn pull_with(env: &Env, name: Option<&str>) -> Result<PullOutcome, CmdError> {
    env.ensure_store()?;

    // Best-effort refresh; failure (offline, detached, etc.) is non-fatal,
    // exactly like the bash `|| true`.
    let _ = Command::new("git")
        .args(["-C"])
        .arg(&env.store_root)
        .args(["pull", "--ff-only", "-q"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let entry = resolve_entry(env, name)?;
    if !entry.exists {
        return Err(CmdError::no_entry(format!(
            "no store entry for {} — run 'secrets add' first",
            entry.rel
        )));
    }

    let dest = env.cwd.join(".env");
    // Decrypt straight into ./.env with mode 600. The plaintext stream is
    // redirected to the file and never enters our address space as a buffer we
    // could print.
    let dest_file = create_private_file(&dest)?;
    let status = Command::new("sops")
        .args(["-d", "--input-type", "dotenv", "--output-type", "dotenv"])
        .arg(&entry.path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(dest_file))
        .stderr(Stdio::inherit())
        .status()
        .map_err(|err| {
            let _ = fs::remove_file(&dest);
            CmdError::unavailable(format!("failed to run sops: {err}"), "sops-unavailable")
        })?;

    if !status.success() {
        // Leave no partial plaintext behind on decryption failure.
        let _ = fs::remove_file(&dest);
        return Err(CmdError::runtime(format!(
            "sops failed to decrypt {}",
            entry.rel
        )));
    }

    // Count keys for metadata ONLY (we read key names from a file we just wrote;
    // values are never surfaced). This is the destination plaintext, but we
    // extract nothing but the count and (optionally) names.
    let key_count = count_dotenv_keys(&dest);

    Ok(PullOutcome {
        entry: entry.rel,
        dest: dest.to_string_lossy().to_string(),
        key_count,
    })
}

fn add_with(env: &Env, file: &str) -> Result<AddOutcome, CmdError> {
    env.ensure_store()?;

    let src = env.cwd.join(file);
    if !src.is_file() {
        return Err(CmdError::no_entry(format!("no '{file}' here to add")));
    }

    // Resolve the slug while CWD is the app repo (matches bash semantics).
    let slug = repo_slug(env)?;
    let rel = format!("repos/{slug}{}", store::ENC_SUFFIX);
    let target = env.store_root.join(&rel);

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CmdError::runtime(format!("cannot create {}: {err}", parent.display()))
        })?;
    }

    // Keep the tracked target absent or unchanged until SOPS has produced and
    // we have validated a complete ciphertext file. The private output lives
    // under `.git`, so even an interrupted run cannot expose a tracked
    // plaintext path or an untracked plaintext sibling beside it.
    let encrypted = PrivateTempFile::new(&env.store_root.join(".git"))?;
    let encrypted_output = encrypted.reopen()?;

    let encrypt_status = Command::new("sops")
        .args([
            "-e",
            "--input-type",
            "dotenv",
            "--output-type",
            "dotenv",
            "--filename-override",
        ])
        .arg(&rel)
        .arg(&src)
        .current_dir(&env.store_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(encrypted_output))
        .stderr(Stdio::inherit())
        .status();

    let encrypted_ok = matches!(&encrypt_status, Ok(status) if status.success());
    // VERIFY the result is actually encrypted before trusting it.
    let looks_encrypted = encrypted_ok && file_contains(encrypted.path(), b"ENC[");

    if !looks_encrypted {
        // `encrypted` removes its private output on drop. The tracked target is
        // still absent or still holds the previous ciphertext.
        return Err(CmdError::runtime(format!(
            "encryption failed for {rel} — target unchanged, nothing committed"
        )));
    }

    encrypted.persist(&target)?;

    // Stage. If nothing changed, report unchanged (no commit/push).
    git_in(&env.store_root, &["add", &rel])?;
    if git_index_clean(&env.store_root, &rel)? {
        return Ok(AddOutcome {
            file: file.to_string(),
            entry: rel,
            committed: false,
            pushed: false,
            note: "unchanged".to_string(),
        });
    }

    // The store repo has no commit hook, so use git commit directly.
    let basename = Path::new(file)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file.to_string());
    let subject = format!("chore(store): add encrypted env for {slug}");
    let body = format!("Encrypt {basename} into the central store as {rel}");
    git_in(&env.store_root, &["commit", "-m", &subject, "-m", &body])?;
    git_in(&env.store_root, &["push", "-q"])?;

    Ok(AddOutcome {
        file: file.to_string(),
        entry: rel,
        committed: true,
        pushed: true,
        note: "encrypted, committed, pushed".to_string(),
    })
}

fn list_with(env: &Env) -> Result<ListOutcome, CmdError> {
    env.ensure_store()?;
    Ok(ListOutcome {
        entries: store::list_entries(&env.store_root),
    })
}

fn which_with(env: &Env, name: Option<&str>) -> Result<WhichOutcome, CmdError> {
    env.ensure_store()?;
    let entry = resolve_entry(env, name)?;
    Ok(WhichOutcome {
        entry: entry.rel,
        path: entry.path.to_string_lossy().to_string(),
        exists: entry.exists,
    })
}

fn edit_with(env: &Env, name: Option<&str>) -> Result<i32, CmdError> {
    env.ensure_store()?;
    let entry = resolve_entry(env, name)?;
    if !entry.exists {
        return Err(CmdError::no_entry(format!("no store entry: {}", entry.rel)));
    }
    // Hand off interactively to sops; inherit stdio so the editor round-trip
    // stays inside sops and never passes through us.
    let status = Command::new("sops")
        .arg(&entry.path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|err| {
            CmdError::unavailable(format!("failed to run sops: {err}"), "sops-unavailable")
        })?;
    Ok(status.code().unwrap_or(exit::RUNTIME))
}

// ------------------------------ git helpers ----------------------------------

fn git_in(store_root: &Path, args: &[&str]) -> Result<(), CmdError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(store_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| CmdError::runtime(format!("failed to run git {}: {err}", args.join(" "))))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(CmdError::runtime(format!(
            "git {} failed: {stderr}",
            args.join(" ")
        )))
    }
}

/// True when nothing is staged for `rel` (the `add` no-op short-circuit).
fn git_index_clean(store_root: &Path, rel: &str) -> Result<bool, CmdError> {
    let status = Command::new("git")
        .args(["diff", "--cached", "--quiet", "--", rel])
        .current_dir(store_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| CmdError::runtime(format!("failed to run git diff: {err}")))?;
    // `git diff --quiet` exits 0 when there is no diff (clean), 1 when there is.
    Ok(status.success())
}

// ------------------------------ fs helpers -----------------------------------

/// Create (or truncate) a file with mode 600 for the decrypted plaintext.
fn create_private_file(path: &Path) -> Result<File, CmdError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|err| CmdError::runtime(format!("cannot write {}: {err}", path.display())))
}

static ADD_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Mode-600 temporary ciphertext output owned by one `secrets add` attempt.
///
/// The path is created with `create_new` below the store's ignored `.git`
/// directory. Drop removes it on SOPS errors, invalid output, and child signal
/// termination. `persist` uses a same-filesystem rename so the tracked target
/// changes atomically from absent/old ciphertext to complete new ciphertext.
struct PrivateTempFile {
    path: PathBuf,
    file: File,
    persisted: bool,
}

impl PrivateTempFile {
    fn new(dir: &Path) -> Result<Self, CmdError> {
        for _ in 0..128 {
            let sequence = ADD_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(
                "secrets-add-{}-{sequence}.enc.env.tmp",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }

            match options.open(&path) {
                Ok(file) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(
                            |err| {
                                let _ = fs::remove_file(&path);
                                CmdError::runtime(format!(
                                    "cannot secure temporary encryption output: {err}"
                                ))
                            },
                        )?;
                    }
                    return Ok(Self {
                        path,
                        file,
                        persisted: false,
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(CmdError::runtime(format!(
                        "cannot create private encryption output: {err}"
                    )));
                }
            }
        }

        Err(CmdError::runtime(
            "cannot allocate a unique private encryption output",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn reopen(&self) -> Result<File, CmdError> {
        self.file.try_clone().map_err(|err| {
            CmdError::runtime(format!("cannot open private encryption output: {err}"))
        })
    }

    fn persist(mut self, target: &Path) -> Result<(), CmdError> {
        self.file.sync_all().map_err(|err| {
            CmdError::runtime(format!(
                "cannot sync encrypted output before install: {err}"
            ))
        })?;
        fs::rename(&self.path, target).map_err(|err| {
            CmdError::runtime(format!(
                "cannot atomically install encrypted output at {}: {err}",
                target.display()
            ))
        })?;
        self.persisted = true;
        Ok(())
    }
}

impl Drop for PrivateTempFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn file_contains(path: &Path, needle: &[u8]) -> bool {
    let Ok(data) = fs::read(path) else {
        return false;
    };
    data.windows(needle.len()).any(|window| window == needle)
}

/// Count dotenv-style `KEY=...` lines. Returns the count of keys only — no
/// values, no key names — purely for metadata reporting.
fn count_dotenv_keys(path: &Path) -> usize {
    let Ok(data) = fs::read_to_string(path) else {
        return 0;
    };
    data.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| {
            line.split_once('=')
                .map(|(key, _)| !key.trim().is_empty())
                .unwrap_or(false)
        })
        .count()
}

// ------------------------------ output emission ------------------------------

/// Per-command success payload. Implementors own both their human text and
/// their JSON `data` (which MUST be metadata-only — never secret values).
trait Outcome {
    fn command(&self) -> &'static str;
    fn human(&self) -> String;
    fn to_json(&self) -> serde_json::Value;
    fn exit_code(&self) -> i32 {
        exit::SUCCESS
    }
}

fn emit_success<T: Outcome>(format: OutputFormat, outcome: T) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(
                schema_version_for(BINARY, outcome.command(), SCHEMA_VERSION),
                outcome.to_json(),
            );
            print_json(&envelope);
        }
        OutputFormat::Text => {
            println!("{}", outcome.human());
        }
    }
    outcome.exit_code()
}

fn emit_error(format: OutputFormat, err: &CmdError) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for(BINARY, "error", SCHEMA_VERSION),
                EnvelopeError::new(err.error_code.clone(), err.message.clone()),
            );
            print_json(&envelope);
        }
        OutputFormat::Text => {
            eprintln!("{BINARY}: {}", err.message);
        }
    }
    err.code
}

fn print_json<T: Serialize>(envelope: &T) {
    match serde_json::to_string(envelope) {
        Ok(line) => {
            let mut stdout = io::stdout().lock();
            let _ = writeln!(stdout, "{line}");
        }
        Err(err) => eprintln!("{BINARY}: failed to serialize JSON: {err}"),
    }
}

// ------------------------------ outcome types --------------------------------

#[derive(Debug, Serialize)]
pub struct PullOutcome {
    pub entry: String,
    pub dest: String,
    pub key_count: usize,
}

impl Outcome for PullOutcome {
    fn command(&self) -> &'static str {
        "pull"
    }
    fn human(&self) -> String {
        format!(
            "{BINARY}: {} -> {} ({} keys)",
            self.entry, self.dest, self.key_count
        )
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "entry": self.entry,
            "dest": self.dest,
            "key_count": self.key_count,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct AddOutcome {
    pub file: String,
    pub entry: String,
    pub committed: bool,
    pub pushed: bool,
    pub note: String,
}

impl Outcome for AddOutcome {
    fn command(&self) -> &'static str {
        "add"
    }
    fn human(&self) -> String {
        if self.committed {
            format!("{BINARY}: {} -> {} ({})", self.file, self.entry, self.note)
        } else {
            format!("{BINARY}: {} {}", self.entry, self.note)
        }
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "file": self.file,
            "entry": self.entry,
            "committed": self.committed,
            "pushed": self.pushed,
            "note": self.note,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct ListOutcome {
    pub entries: Vec<String>,
}

impl Outcome for ListOutcome {
    fn command(&self) -> &'static str {
        "list"
    }
    fn human(&self) -> String {
        self.entries.join("\n")
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "entries": self.entries })
    }
}

#[derive(Debug, Serialize)]
pub struct WhichOutcome {
    pub entry: String,
    pub path: String,
    pub exists: bool,
}

impl Outcome for WhichOutcome {
    fn command(&self) -> &'static str {
        "which"
    }
    fn human(&self) -> String {
        self.path.clone()
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "entry": self.entry,
            "path": self.path,
            "exists": self.exists,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn count_dotenv_keys_ignores_comments_and_blanks() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join(".env");
        fs::write(&path, "# comment\n\nA=1\nB=secret\n  C = 3\nnotakey\n").expect("write");
        assert_eq!(count_dotenv_keys(&path), 3);
    }

    #[test]
    fn file_contains_detects_marker() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("x");
        fs::write(&path, "A=ENC[AES256_GCM,data:...]").expect("write");
        assert!(file_contains(&path, b"ENC["));
        fs::write(&path, "A=plaintext").expect("write");
        assert!(!file_contains(&path, b"ENC["));
    }

    #[test]
    fn pull_outcome_json_is_metadata_only() {
        let outcome = PullOutcome {
            entry: "repos/owner/repo.enc.env".to_string(),
            dest: "/work/.env".to_string(),
            key_count: 4,
        };
        let json = outcome.to_json();
        assert_eq!(json["entry"], "repos/owner/repo.enc.env");
        assert_eq!(json["key_count"], 4);
        // No value-bearing field exists.
        assert!(json.get("value").is_none());
        assert!(json.get("env").is_none());
    }
}
