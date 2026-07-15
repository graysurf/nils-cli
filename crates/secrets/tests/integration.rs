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
/// the secret canary) to stdout. For `-e`, it supports both the retired in-place
/// shape and the safe stdout-output shape so the tests can prove the final
/// target's state while encryption is running. Behavior is steered by env vars.
fn sops_stub(dir: &Path) {
    write_exe(
        dir,
        "sops",
        r#"#!/bin/bash
printf '%s\n' "$*" >> "${SOPS_LOG:-/dev/null}"

mode=""
file=""
filename_override=""
inplace=0
while (( "$#" )); do
  case "$1" in
    -d) mode="d" ;;
    -e) mode="e" ;;
    -i) inplace=1 ;;
    --filename-override)
      shift
      filename_override="${1:-}"
      ;;
    --input-type|--output-type)
      shift
      ;;
    -*) ;;
    *) file="$1" ;;
  esac
  shift
done

if [[ "$mode" == "e" ]]; then
  final_target="${SOPS_STORE:-}/${filename_override:-$file}"

  case "${SOPS_ASSERT_FINAL_STATE:-}" in
    absent)
      if [[ -e "$final_target" ]]; then
        echo "sops: final target existed during encryption" >&2
        exit 1
      fi
      ;;
    unchanged)
      if [[ ! -f "$final_target" ]] ||
         [[ "$(<"$final_target")" != "${SOPS_EXPECTED_TARGET:-}" ]]; then
        echo "sops: final target changed during encryption" >&2
        exit 1
      fi
      ;;
  esac

  if [[ "${SOPS_REQUIRE_FILENAME_OVERRIDE:-0}" == "1" && -z "$filename_override" ]]; then
    echo "sops: missing filename override" >&2
    exit 1
  fi

  if [[ "${SOPS_ASSERT_OUTPUT_MODE:-0}" == "1" ]]; then
    case "$(uname -s)" in
      Darwin|FreeBSD) output_mode="$(stat -f '%Lp' /dev/fd/1 2>/dev/null || true)" ;;
      *) output_mode="$(stat -Lc '%a' /dev/fd/1 2>/dev/null || true)" ;;
    esac
    if [[ "$output_mode" != "600" ]]; then
      echo "sops: encryption output is not mode 600" >&2
      exit 1
    fi
  fi

  if [[ "${SOPS_SIGNAL:-0}" == "1" ]]; then
    kill -TERM "$$"
  fi
fi

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

if [[ "$mode" == "e" ]]; then
  # Safe encryption shape: consume the source path and emit ciphertext only to
  # stdout, which the CLI redirects to its private temporary output.
  if [[ ! -f "$file" ]]; then
    echo "sops: missing plaintext input" >&2
    exit 1
  fi
  if [[ "${SOPS_NO_ENC:-0}" == "1" ]]; then
    printf 'API_KEY=TOP-SECRET-VALUE\n'
  else
    printf 'API_KEY=ENC[AES256_GCM,data:abc,type:str]\n'
  fi
  exit 0
fi

# Bare `sops <file>` (edit): no-op success.
exit 0
"#,
    );
}

fn assert_no_add_temp_files(store: &Path) {
    let leftovers: Vec<_> = fs::read_dir(store.join(".git"))
        .expect("read store .git")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("secrets-add-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "secrets add temporary files were not cleaned up: {leftovers:?}"
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

    // secrets is a `completion_engine=dynamic` CLI: the exported scripts are
    // clap_complete `CompleteEnv` registration stubs, not static `generate()`
    // scripts. The dynamic completer calls back into the binary at TAB time to
    // enumerate live store entry names.
    let zsh = run(
        &["completion", "zsh"],
        &options(tmp.path(), &store, stubs.path()),
    );
    assert_exit(&zsh, 0);
    let zsh_text = zsh.stdout_text();
    assert!(
        zsh_text.contains("#compdef secrets"),
        "dynamic zsh registration keeps the #compdef header"
    );
    assert!(
        zsh_text.contains("_clap_dynamic_completer_secrets"),
        "dynamic zsh registration defines the CompleteEnv completer function"
    );
    assert!(
        zsh_text.contains("compdef _clap_dynamic_completer_secrets secrets"),
        "dynamic zsh registration binds the completer to secrets"
    );
    assert!(
        !zsh_text.contains("_arguments"),
        "dynamic stub must not embed the static `_arguments` surface"
    );

    let bash = run(
        &["completion", "bash"],
        &options(tmp.path(), &store, stubs.path()),
    );
    assert_exit(&bash, 0);
    let bash_text = bash.stdout_text();
    assert!(
        bash_text.contains("_clap_complete_secrets"),
        "dynamic bash registration defines the CompleteEnv completer function"
    );
    assert!(
        bash_text.contains("-F _clap_complete_secrets secrets"),
        "dynamic bash registration binds the completer to secrets via complete -F"
    );
}

#[test]
fn dynamic_completion_enumerates_live_store_entries() {
    let tmp = TempDir::new().expect("tmp");
    let store = tmp.path().join("store");
    init_store(&store);
    fs::create_dir_all(store.join("repos/graysurf")).expect("repos/graysurf");
    fs::write(store.join("repos/graysurf/g14-infra.enc.env"), "x").expect("repo entry");
    fs::write(store.join("stacks/web.enc.env"), "x").expect("stack web");
    fs::write(store.join("stacks/db.enc.env"), "x").expect("stack db");

    // Drive the clap_complete `CompleteEnv` runtime completer directly: with
    // `COMPLETE=zsh` and the cursor on the `name` positional, the binary should
    // print the live store entry names attached via `#[arg(add = ...)]`.
    let opts = options(tmp.path(), &store, tmp.path())
        .with_env("COMPLETE", "zsh")
        .with_env("_CLAP_COMPLETE_INDEX", "2")
        .with_env("_CLAP_IFS", "\n");
    let output = run(&["--", "secrets", "pull", ""], &opts);
    assert_exit(&output, 0);
    let stdout = output.stdout_text();

    for expected in ["repos/graysurf/g14-infra", "stacks/db", "stacks/web"] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "name completion should offer `{expected}`, got:\n{stdout}"
        );
    }
    assert_no_secret_leak(&output);
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
    let sops_log = tmp.path().join("sops.log");
    fs::write(&git_log, "").expect("git log");
    fs::write(&sops_log, "").expect("sops log");
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(
        &["add"],
        &options(&app, &store, stubs.path())
            .with_env("GIT_LOG", &git_log.to_string_lossy())
            .with_env("SOPS_LOG", &sops_log.to_string_lossy())
            .with_env("SOPS_STORE", &store.to_string_lossy())
            .with_env("SOPS_ASSERT_FINAL_STATE", "absent")
            .with_env("SOPS_ASSERT_OUTPUT_MODE", "1")
            .with_env("SOPS_REQUIRE_FILENAME_OVERRIDE", "1"),
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

    let sops_calls = fs::read_to_string(&sops_log).expect("sops log");
    assert!(
        sops_calls.contains("--filename-override repos/graysurf/g14-infra.enc.env"),
        "sops must evaluate creation rules against the final target path: {sops_calls}"
    );
    assert!(
        !sops_calls.split_whitespace().any(|arg| arg == "-i"),
        "add must not encrypt the tracked target in place: {sops_calls}"
    );
    assert_no_add_temp_files(&store);

    let stdout = output.stdout_text();
    assert!(stdout.contains("repos/graysurf/g14-infra.enc.env"));
}

#[test]
fn add_non_ciphertext_output_leaves_no_target_or_temp() {
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
            .with_env("SOPS_STORE", &store.to_string_lossy())
            .with_env("SOPS_ASSERT_FINAL_STATE", "absent")
            .with_env("SOPS_REQUIRE_FILENAME_OVERRIDE", "1")
            // sops succeeds but leaves plaintext (no ENC marker) -> must abort.
            .with_env("SOPS_NO_ENC", "1"),
    );
    assert_exit(&output, 1);
    assert!(output.stderr_text().contains("encryption failed"));
    assert_no_secret_leak(&output);

    // Plaintext copy removed; nothing staged/committed/pushed.
    assert!(
        !store.join("repos/graysurf/g14-infra.enc.env").exists(),
        "invalid encryption output must not create the final target"
    );
    let git_calls = fs::read_to_string(&git_log).expect("git log");
    assert!(!git_calls.contains("commit"), "must not commit on failure");
    assert!(!git_calls.contains("push"), "must not push on failure");
    assert_no_add_temp_files(&store);
}

#[test]
fn add_unchanged_stages_without_committing_or_pushing() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    fs::write(app.join(".env"), format!("API_KEY={SECRET_CANARY}\n")).expect("write src");

    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    let target = store.join("repos/graysurf/g14-infra.enc.env");
    fs::create_dir_all(target.parent().expect("target parent")).expect("target parent");
    let ciphertext = "API_KEY=ENC[AES256_GCM,data:abc,type:str]\n";
    fs::write(&target, ciphertext).expect("existing ciphertext");
    let git_log = tmp.path().join("git.log");
    fs::write(&git_log, "").expect("git log");
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(
        &["add"],
        &options(&app, &store, stubs.path())
            .with_env("GIT_LOG", &git_log.to_string_lossy())
            .with_env("GIT_DIFF_CLEAN", "1")
            .with_env("SOPS_STORE", &store.to_string_lossy())
            .with_env("SOPS_ASSERT_FINAL_STATE", "unchanged")
            .with_env("SOPS_EXPECTED_TARGET", ciphertext.trim_end())
            .with_env("SOPS_ASSERT_OUTPUT_MODE", "1")
            .with_env("SOPS_REQUIRE_FILENAME_OVERRIDE", "1"),
    );
    assert_exit(&output, 0);
    assert_no_secret_leak(&output);
    assert!(output.stdout_text().contains("unchanged"));
    assert_eq!(
        fs::read_to_string(&target).expect("ciphertext retained"),
        ciphertext
    );

    let git_calls = fs::read_to_string(&git_log).expect("git log");
    assert!(git_calls.contains("add repos/graysurf/g14-infra.enc.env"));
    assert!(
        !git_calls.contains("commit"),
        "must not commit unchanged data"
    );
    assert!(!git_calls.contains("push"), "must not push unchanged data");
    assert_no_add_temp_files(&store);
}

#[test]
fn add_sops_failure_preserves_existing_ciphertext_and_cleans_temp() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    fs::write(app.join(".env"), format!("API_KEY={SECRET_CANARY}\n")).expect("write src");

    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    let target = store.join("repos/graysurf/g14-infra.enc.env");
    fs::create_dir_all(target.parent().expect("target parent")).expect("target parent");
    let original = "API_KEY=ENC[AES256_GCM,data:original,type:str]\n";
    fs::write(&target, original).expect("existing ciphertext");
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(
        &["add"],
        &options(&app, &store, stubs.path())
            .with_env("SOPS_STORE", &store.to_string_lossy())
            .with_env("SOPS_ASSERT_FINAL_STATE", "unchanged")
            .with_env("SOPS_EXPECTED_TARGET", original.trim_end())
            .with_env("SOPS_REQUIRE_FILENAME_OVERRIDE", "1")
            .with_env("SOPS_FAIL", "1"),
    );
    assert_exit(&output, 1);
    assert_no_secret_leak(&output);
    assert_eq!(
        fs::read_to_string(&target).expect("existing ciphertext retained"),
        original
    );
    assert_no_add_temp_files(&store);
}

#[test]
fn add_non_ciphertext_output_preserves_existing_target_and_cleans_temp() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    fs::write(app.join(".env"), format!("API_KEY={SECRET_CANARY}\n")).expect("write src");

    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    let target = store.join("repos/graysurf/g14-infra.enc.env");
    fs::create_dir_all(target.parent().expect("target parent")).expect("target parent");
    let original = "API_KEY=ENC[AES256_GCM,data:original,type:str]\n";
    fs::write(&target, original).expect("existing ciphertext");
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(
        &["add"],
        &options(&app, &store, stubs.path())
            .with_env("SOPS_STORE", &store.to_string_lossy())
            .with_env("SOPS_ASSERT_FINAL_STATE", "unchanged")
            .with_env("SOPS_EXPECTED_TARGET", original.trim_end())
            .with_env("SOPS_REQUIRE_FILENAME_OVERRIDE", "1")
            .with_env("SOPS_NO_ENC", "1"),
    );
    assert_exit(&output, 1);
    assert_no_secret_leak(&output);
    assert_eq!(
        fs::read_to_string(&target).expect("existing ciphertext retained"),
        original
    );
    assert_no_add_temp_files(&store);
}

#[test]
fn add_cleans_temp_when_sops_is_terminated_by_signal() {
    let tmp = TempDir::new().expect("tmp");
    let app = tmp.path().join("app");
    fs::create_dir_all(&app).expect("app dir");
    fs::write(app.join(".env"), format!("API_KEY={SECRET_CANARY}\n")).expect("write src");

    let stubs = StubBinDir::new();
    let store = tmp.path().join("store");
    init_store(&store);
    git_stub(stubs.path());
    sops_stub(stubs.path());

    let output = run(
        &["add"],
        &options(&app, &store, stubs.path())
            .with_env("SOPS_STORE", &store.to_string_lossy())
            .with_env("SOPS_ASSERT_FINAL_STATE", "absent")
            .with_env("SOPS_REQUIRE_FILENAME_OVERRIDE", "1")
            .with_env("SOPS_SIGNAL", "1"),
    );
    assert_exit(&output, 1);
    assert_no_secret_leak(&output);
    assert!(
        !store.join("repos/graysurf/g14-infra.enc.env").exists(),
        "signal failure must not create the final target"
    );
    assert_no_add_temp_files(&store);
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
