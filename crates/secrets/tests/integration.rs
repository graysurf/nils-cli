//! Hermetic integration tests for the `secrets` CLI.
//!
//! Every test runs the real `secrets` binary but with stubbed `git`/`sops` on
//! PATH and `SECRETS_REPO` pointed at a tempdir store, so nothing touches the
//! network, a real SOPS key, or a real remote. The SECRET-VALUE marker
//! `TOP-SECRET-VALUE` is used as a canary: it is written into the decrypted
//! `.env` (or the plaintext add source) and every test asserts it never appears
//! in stdout or the JSON envelope.

use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::{StubBinDir, bin, write_exe};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Canary string standing in for a decrypted secret value. It must NEVER reach
/// stdout or the JSON envelope.
const SECRET_CANARY: &str = "TOP-SECRET-VALUE";

fn secrets_bin() -> PathBuf {
    bin::resolve("secrets")
}

fn run(args: &[&str], options: &CmdOptions) -> CmdOutput {
    cmd::run_with(&secrets_bin(), args, options)
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code, "stderr: {}", output.stderr_text());
}

/// Assert the secret canary never leaked to stdout or stderr.
fn assert_no_secret_leak(output: &CmdOutput) {
    assert!(
        !output.stdout_text().contains(SECRET_CANARY),
        "secret value leaked to stdout: {}",
        output.stdout_text()
    );
    assert!(
        !output.stderr_text().contains(SECRET_CANARY),
        "secret value leaked to stderr: {}",
        output.stderr_text()
    );
}

/// Initialize a fake store at `store` (a `.git` marker + optional entries).
fn init_store(store: &Path) {
    fs::create_dir_all(store.join(".git")).expect("store .git");
    fs::create_dir_all(store.join("repos")).expect("store repos");
    fs::create_dir_all(store.join("stacks")).expect("store stacks");
}

/// Base options: CWD in `app_repo`, store via SECRETS_REPO, stubs on PATH.
fn options(app_repo: &Path, store: &Path, stubs: &Path) -> CmdOptions {
    CmdOptions::default()
        .with_cwd(app_repo)
        .with_path_prepend(stubs)
        .with_env("SECRETS_REPO", &store.to_string_lossy())
        .with_env_remove("HOME")
}

/// Write a git stub that satisfies every git invocation the CLI makes:
/// `remote get-url origin`, `-C <dir> pull ...`, `add`, `diff --cached --quiet`,
/// `commit`, and `push`. Behavior is steered by env vars so individual tests can
/// vary it. All operations are logged to `$GIT_LOG`.
fn git_stub(dir: &Path) {
    write_exe(
        dir,
        "git",
        r#"#!/bin/bash
# Log the full argv for assertions.
printf '%s\n' "$*" >> "${GIT_LOG:-/dev/null}"

# `git -C <dir> ...` — drop the -C and its arg, behave as no-op (pull refresh).
if [[ "$1" == "-C" ]]; then
  exit 0
fi

case "$1 $2" in
  "remote get-url")
    printf '%s\n' "${GIT_ORIGIN_URL:-git@github.com:graysurf/g14-infra.git}"
    exit 0
    ;;
esac

case "$1" in
  add|commit|push)
    exit 0
    ;;
  diff)
    # `git diff --cached --quiet -- <rel>`: exit 0 = no diff (clean), 1 = diff.
    # Tests set GIT_DIFF_CLEAN=1 to simulate "nothing changed".
    if [[ "${GIT_DIFF_CLEAN:-0}" == "1" ]]; then exit 0; else exit 1; fi
    ;;
esac
exit 0
"#,
    );
}

/// Write a sops stub. For `-d` (decrypt) it writes a fake plaintext dotenv (with
/// the secret canary) to stdout. For `-e -i` (encrypt in place) it rewrites the
/// target file with an `ENC[...]` marker. Behavior is steered by env vars.
fn sops_stub(dir: &Path) {
    write_exe(
        dir,
        "sops",
        r#"#!/bin/bash
printf '%s\n' "$*" >> "${SOPS_LOG:-/dev/null}"

mode=""
file=""
inplace=0
for a in "$@"; do
  case "$a" in
    -d) mode="d" ;;
    -e) mode="e" ;;
    -i) inplace=1 ;;
    -*) ;;
    dotenv) ;;
    *) file="$a" ;;
  esac
done

if [[ "${SOPS_FAIL:-0}" == "1" ]]; then
  echo "sops: simulated failure" >&2
  exit 1
fi

if [[ "$mode" == "d" ]]; then
  # Decrypt: emit fake plaintext to stdout (the CLI redirects this into .env).
  printf 'API_KEY=TOP-SECRET-VALUE\nDB_URL=postgres://localhost/db\n# comment\n'
  exit 0
fi

if [[ "$mode" == "e" && "$inplace" == "1" ]]; then
  # Encrypt in place: overwrite target with an ENC marker, unless told not to.
  if [[ "${SOPS_NO_ENC:-0}" == "1" ]]; then
    printf 'API_KEY=TOP-SECRET-VALUE\n' > "$file"
  else
    printf 'API_KEY=ENC[AES256_GCM,data:abc,type:str]\n' > "$file"
  fi
  exit 0
fi

# Bare `sops <file>` (edit): no-op success.
exit 0
"#,
    );
}

// --------------------------------- tests -------------------------------------

#[test]
fn no_args_prints_help_and_exits_zero() {
    let tmp = TempDir::new().expect("tmp");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(&[], &options(tmp.path(), &store, stubs.path()));
    assert_exit(&output, 0);
    let stdout = output.stdout_text();
    assert!(stdout.contains("central SOPS store"));
    assert!(stdout.contains("pull"));
    assert!(stdout.contains("completion"));
}

#[test]
fn unknown_command_exits_64() {
    let tmp = TempDir::new().expect("tmp");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);

    let output = run(&["nope"], &options(tmp.path(), &store, stubs.path()));
    assert_exit(&output, 64);
    assert!(output.stderr_text().contains("unrecognized subcommand"));
}

#[test]
fn unknown_command_json_emits_error_envelope() {
    let tmp = TempDir::new().expect("tmp");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);

    let output = run(
        &["--format", "json", "nope"],
        &options(tmp.path(), &store, stubs.path()),
    );
    assert_exit(&output, 64);
    let json = output.stdout_json();
    assert_eq!(json["ok"], false);
    assert_eq!(json["schema_version"], "cli.secrets.error.v1");
    assert_eq!(json["error"]["code"], "invalid-arguments");
}

#[test]
fn completion_exports_bash_and_zsh() {
    let tmp = TempDir::new().expect("tmp");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);

    let zsh = run(
        &["completion", "zsh"],
        &options(tmp.path(), &store, stubs.path()),
    );
    assert_exit(&zsh, 0);
    let zsh_text = zsh.stdout_text();
    assert!(zsh_text.contains("#compdef secrets"));
    assert!(zsh_text.contains("pull:"));

    let bash = run(
        &["completion", "bash"],
        &options(tmp.path(), &store, stubs.path()),
    );
    assert_exit(&bash, 0);
    let bash_text = bash.stdout_text();
    assert!(bash_text.contains("_secrets()"));
    assert!(bash_text.contains("complete -F _secrets"));
}

#[test]
fn which_resolves_auto_detected_slug() {
    let tmp = TempDir::new().expect("tmp");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(&["which"], &options(tmp.path(), &store, stubs.path()));
    assert_exit(&output, 0);
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("repos/graysurf/g14-infra.enc.env"),
        "stdout: {stdout}"
    );
    assert_no_secret_leak(&output);
}

#[test]
fn which_json_envelope_is_metadata_only() {
    let tmp = TempDir::new().expect("tmp");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(
        &["--format", "json", "which", "my-stack"],
        &options(tmp.path(), &store, stubs.path()),
    );
    assert_exit(&output, 0);
    let json = output.stdout_json();
    assert_eq!(json["ok"], true);
    assert_eq!(json["schema_version"], "cli.secrets.which.v1");
    assert_eq!(json["data"]["entry"], "repos/my-stack.enc.env");
    assert_eq!(json["data"]["exists"], false);
    assert_no_secret_leak(&output);
}

#[test]
fn list_returns_entry_names_only() {
    let tmp = TempDir::new().expect("tmp");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    // Entries that, if their CONTENTS leaked, would expose the canary.
    fs::create_dir_all(store.join("repos/owner")).expect("mkdir");
    fs::write(
        store.join("repos/owner/repo.enc.env"),
        format!("A=ENC[{SECRET_CANARY}]"),
    )
    .expect("write");
    fs::write(
        store.join("stacks/web.enc.env"),
        format!("B=ENC[{SECRET_CANARY}]"),
    )
    .expect("write");

    let text = run(&["list"], &options(tmp.path(), &store, stubs.path()));
    assert_exit(&text, 0);
    assert_eq!(text.stdout_text().trim(), "repos/owner/repo\nstacks/web");
    assert_no_secret_leak(&text);

    let json = run(
        &["--format", "json", "list"],
        &options(tmp.path(), &store, stubs.path()),
    );
    assert_exit(&json, 0);
    let parsed = json.stdout_json();
    assert_eq!(parsed["schema_version"], "cli.secrets.list.v1");
    assert_eq!(parsed["data"]["entries"][0], "repos/owner/repo");
    assert_eq!(parsed["data"]["entries"][1], "stacks/web");
    assert_no_secret_leak(&json);
}

#[test]
fn pull_writes_dotenv_600_without_leaking_secret() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    git_stub(stubs.path());
    sops_stub(stubs.path());
    // The store entry for the auto-detected slug must exist.
    fs::create_dir_all(store.join("repos/graysurf")).expect("mkdir");
    fs::write(
        store.join("repos/graysurf/g14-infra.enc.env"),
        format!("A=ENC[{SECRET_CANARY}]"),
    )
    .expect("write");

    let output = run(&["pull"], &options(&app, &store, stubs.path()));
    assert_exit(&output, 0);
    assert_no_secret_leak(&output);

    // .env was written and contains the decrypted plaintext (the canary lives
    // ON DISK, which is the whole point) but it never reached stdout.
    let dotenv = app.join(".env");
    let written = fs::read_to_string(&dotenv).expect("read .env");
    assert!(written.contains(SECRET_CANARY), "{written}");

    // Metadata-only stdout: store rel path + key count, no values.
    let stdout = output.stdout_text();
    assert!(stdout.contains("repos/graysurf/g14-infra.enc.env"));
    assert!(stdout.contains("2 keys"), "stdout: {stdout}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&dotenv).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "expected .env mode 600");
    }
}

#[test]
fn pull_json_envelope_is_metadata_only() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    git_stub(stubs.path());
    sops_stub(stubs.path());
    fs::create_dir_all(store.join("repos/graysurf")).expect("mkdir");
    fs::write(store.join("repos/graysurf/g14-infra.enc.env"), "x").expect("write");

    let output = run(
        &["--format", "json", "pull"],
        &options(&app, &store, stubs.path()),
    );
    assert_exit(&output, 0);
    let json = output.stdout_json();
    assert_eq!(json["ok"], true);
    assert_eq!(json["schema_version"], "cli.secrets.pull.v1");
    assert_eq!(json["data"]["entry"], "repos/graysurf/g14-infra.enc.env");
    assert_eq!(json["data"]["key_count"], 2);
    // No value-bearing field can carry a secret.
    assert!(json["data"].get("value").is_none());
    assert!(json["data"].get("env").is_none());
    assert!(json["data"].get("keys").is_none());
    assert_no_secret_leak(&output);
}

#[test]
fn pull_missing_entry_exits_65() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(&["pull"], &options(&app, &store, stubs.path()));
    assert_exit(&output, 65);
    assert!(output.stderr_text().contains("run 'secrets add' first"));
    assert!(!app.join(".env").exists(), ".env must not be created");
}

#[test]
fn add_encrypts_commits_and_pushes() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    // Plaintext source carrying the secret canary.
    fs::write(app.join(".env"), format!("API_KEY={SECRET_CANARY}\n")).expect("write src");

    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    let git_log = tmp.path().join("git.log");
    fs::write(&git_log, "").expect("git log");
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(
        &["add"],
        &options(&app, &store, stubs.path()).with_env("GIT_LOG", &git_log.to_string_lossy()),
    );
    assert_exit(&output, 0);
    assert_no_secret_leak(&output);

    // The stored file is the ENC-marked output, not plaintext.
    let stored =
        fs::read_to_string(store.join("repos/graysurf/g14-infra.enc.env")).expect("read stored");
    assert!(stored.contains("ENC["), "stored: {stored}");
    assert!(
        !stored.contains(SECRET_CANARY),
        "plaintext persisted: {stored}"
    );

    // git add + commit + push were all invoked.
    let git_calls = fs::read_to_string(&git_log).expect("git log");
    assert!(git_calls.contains("add repos/graysurf/g14-infra.enc.env"));
    assert!(git_calls.contains("commit"));
    assert!(git_calls.contains("push"));

    let stdout = output.stdout_text();
    assert!(stdout.contains("repos/graysurf/g14-infra.enc.env"));
}

#[test]
fn add_failed_encryption_removes_plaintext_and_aborts() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    fs::write(app.join(".env"), format!("API_KEY={SECRET_CANARY}\n")).expect("write src");

    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    let git_log = tmp.path().join("git.log");
    fs::write(&git_log, "").expect("git log");
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(
        &["add"],
        &options(&app, &store, stubs.path())
            .with_env("GIT_LOG", &git_log.to_string_lossy())
            // sops succeeds but leaves plaintext (no ENC marker) -> must abort.
            .with_env("SOPS_NO_ENC", "1"),
    );
    assert_exit(&output, 1);
    assert!(output.stderr_text().contains("encryption failed"));
    assert_no_secret_leak(&output);

    // Plaintext copy removed; nothing staged/committed/pushed.
    assert!(
        !store.join("repos/graysurf/g14-infra.enc.env").exists(),
        "plaintext copy must be removed on failure"
    );
    let git_calls = fs::read_to_string(&git_log).expect("git log");
    assert!(!git_calls.contains("commit"), "must not commit on failure");
    assert!(!git_calls.contains("push"), "must not push on failure");
}

#[test]
fn add_missing_source_exits_65() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(&["add"], &options(&app, &store, stubs.path()));
    assert_exit(&output, 65);
    assert!(output.stderr_text().contains("no '.env' here to add"));
}

#[test]
fn missing_store_exits_69() {
    let tmp = TempDir::new().expect("tmp");
    let stubs = StubBinDir::new();
    git_stub(stubs.path());
    sops_stub(stubs.path());
    // SECRETS_REPO points at a dir without a .git marker.
    let store = tmp.path().join("not-a-store");
    fs::create_dir_all(&store).expect("dir");

    let output = run(&["list"], &options(tmp.path(), &store, stubs.path()));
    assert_exit(&output, 69);
    assert!(output.stderr_text().contains("store not found"));
}
