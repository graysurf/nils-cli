//! Governed creation of a new private Forgejo repository and signed root commit.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend::{ProcessOutputError, output_with_limits, redact_and_tail};
use crate::cli::{BINARY, GlobalFlags, RepoBootstrapArgs, RepoBootstrapOwnerKind};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::forgejo::ForgejoClient;

const RECEIPT_SCHEMA: &str = "forge-cli.repo-bootstrap.receipt.v1";
const MAX_RECEIPT_BYTES: usize = 256 * 1024;
const MAX_REASON_BYTES: usize = 2_000;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TOTAL_FILE_BYTES: u64 = 16 * 1024 * 1024;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESS_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const GIT_BIN_ENV: &str = "FORGE_CLI_GIT_BIN";
const SEMANTIC_COMMIT_BIN_ENV: &str = "FORGE_CLI_SEMANTIC_COMMIT_BIN";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReceiptFile {
    source: String,
    name: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapReceipt {
    schema_version: String,
    provider: String,
    repository: String,
    owner_kind: RepoBootstrapOwnerKind,
    default_branch: String,
    message: String,
    reason: String,
    files: Vec<ReceiptFile>,
    create_attempted: bool,
    remote_created: bool,
    local_sha: Option<String>,
    push_attempted: bool,
    default_branch_set: bool,
    complete: bool,
    reconciled: bool,
}

#[derive(Debug, Serialize)]
struct BootstrapPayload {
    provider: String,
    repository: String,
    private: bool,
    default_branch: String,
    root_commit_sha: String,
    signature_verified: bool,
    remote_created: bool,
    pushed: bool,
    reconciled: bool,
    idempotent: bool,
    checkout: String,
    receipt: String,
}

#[derive(Debug)]
struct RepoSnapshot {
    clone_url: String,
    default_branch: String,
    empty: bool,
}

#[derive(Debug)]
struct ProcessResult {
    success: bool,
    code: i32,
    stdout: String,
    stderr: String,
}

pub fn run(
    global: &GlobalFlags,
    args: RepoBootstrapArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let (owner, repo) = crate::forgejo::repo_parts(global)?;
    validate_branch(&args.default_branch)?;
    validate_message(&args.message)?;
    let reason = read_small_regular_file(&args.reason_file, MAX_REASON_BYTES, "authorization")?;
    let files = prepare_files(&args.files)?;
    let repository = format!("{owner}/{repo}");
    let provider = global.named_provider().ok_or_else(|| {
        ForgeError::provider_unsupported(
            error_schema(),
            "repo bootstrap requires a named Forgejo provider",
            None,
        )
    })?;
    let state_dir = bootstrap_state_dir(provider, &owner, &repo)?;
    let receipt_path = state_dir.join("receipt.json");
    let checkout = state_dir.join("checkout");

    let client = ForgejoClient::from_global(global)?;
    client.discover_version()?;
    let authenticated_login = client.authenticated_user()?;
    if args.owner_kind == RepoBootstrapOwnerKind::User && authenticated_login != owner {
        return Err(validation(
            "bootstrap_owner_mismatch",
            format!("user-owned bootstrap requires repository owner '{authenticated_login}'"),
            None,
        ));
    }

    let expected = BootstrapReceipt {
        schema_version: RECEIPT_SCHEMA.to_string(),
        provider: provider.to_string(),
        repository: repository.clone(),
        owner_kind: args.owner_kind,
        default_branch: args.default_branch.clone(),
        message: args.message.clone(),
        reason,
        files,
        create_attempted: false,
        remote_created: false,
        local_sha: None,
        push_attempted: false,
        default_branch_set: false,
        complete: false,
        reconciled: false,
    };

    let (mut receipt, idempotent_candidate) = match load_receipt(&receipt_path)? {
        Some(receipt) => {
            if !args.resume {
                return Err(validation(
                    "bootstrap_resume_required",
                    "a bootstrap receipt already exists; pass --resume after inspecting it",
                    Some(receipt_path.display().to_string()),
                ));
            }
            validate_receipt_inputs(&receipt, &expected)?;
            let complete = receipt.complete;
            (receipt, complete)
        }
        None if args.resume => {
            return Err(validation(
                "bootstrap_receipt_missing",
                "--resume requires an existing durable bootstrap receipt",
                Some(receipt_path.display().to_string()),
            ));
        }
        None => {
            if client.repo_optional(&owner, &repo)?.is_some() {
                return Err(validation(
                    "repository_exists",
                    format!("Forgejo repository '{repository}' already exists"),
                    None,
                ));
            }
            create_receipt(&receipt_path, &expected)?;
            (expected, false)
        }
    };

    let mut repo_snapshot = client.repo_optional(&owner, &repo)?;
    if repo_snapshot.is_none() {
        if receipt.remote_created || receipt.complete {
            return Err(validation(
                "remote_drift",
                "the durable bootstrap receipt expects a remote repository, but it is absent",
                None,
            ));
        }
        receipt.create_attempted = true;
        persist_receipt(&receipt_path, &receipt)?;
        let create_result = client.create_repo(args.owner_kind, &owner, &repo);
        repo_snapshot = client.repo_optional(&owner, &repo)?;
        if repo_snapshot.is_none() {
            let detail = create_result.err().map(|error| error.to_string());
            return Err(ForgeError::runtime_failure(
                error_schema(),
                "bootstrap_create_failed",
                "Forgejo repository creation could not be reconciled by exact read-back",
                detail,
            ));
        }
        if create_result.is_err() {
            receipt.reconciled = true;
        }
    } else if !receipt.create_attempted && !receipt.remote_created {
        return Err(validation(
            "repository_exists",
            format!("Forgejo repository '{repository}' appeared before the create attempt"),
            None,
        ));
    }

    let mut snapshot = parse_repo_snapshot(
        repo_snapshot.as_ref().expect("repository snapshot"),
        &client,
        &owner,
        &repo,
    )?;
    let existing_branch =
        branch_sha(client.branch_optional(&owner, &repo, &args.default_branch)?)?;
    if receipt.local_sha.is_none()
        && (!snapshot.empty || existing_branch.is_some() || !snapshot.default_branch.is_empty())
    {
        return Err(validation(
            "repository_not_empty",
            "Forgejo bootstrap requires an empty repository with no branch or default branch",
            None,
        ));
    }
    receipt.remote_created = true;
    persist_receipt(&receipt_path, &receipt)?;

    let local_sha = match receipt.local_sha.clone() {
        Some(sha) => sha,
        None => {
            let sha = match recover_signed_root(
                &checkout,
                &receipt.files,
                &receipt.message,
                &receipt.default_branch,
            )? {
                Some(sha) => sha,
                None => create_signed_root(
                    &checkout,
                    &receipt.files,
                    &receipt.message,
                    &receipt.default_branch,
                )?,
            };
            receipt.local_sha = Some(sha.clone());
            persist_receipt(&receipt_path, &receipt)?;
            sha
        }
    };

    let branch_before_push =
        branch_sha(client.branch_optional(&owner, &repo, &receipt.default_branch)?)?;
    let mut pushed = false;
    match branch_before_push {
        Some(ref observed) if observed == &local_sha => receipt.reconciled = true,
        Some(observed) => return Err(remote_drift(&local_sha, &observed)),
        None => {
            receipt.push_attempted = true;
            persist_receipt(&receipt_path, &receipt)?;
            let askpass = write_askpass(&state_dir)?;
            let push = push_once(
                &checkout,
                &snapshot.clone_url,
                &receipt.default_branch,
                &local_sha,
                &askpass,
                client.token_env(),
                &authenticated_login,
            )?;
            let observed =
                branch_sha(client.branch_optional(&owner, &repo, &receipt.default_branch)?)?;
            match observed {
                Some(ref observed) if observed == &local_sha => {
                    pushed = push.success;
                    if !push.success {
                        receipt.reconciled = true;
                    }
                }
                Some(observed) => return Err(remote_drift(&local_sha, &observed)),
                None => {
                    return Err(ForgeError::runtime_failure(
                        error_schema(),
                        "bootstrap_push_failed",
                        "the bounded first push failed and read-back found no matching branch",
                        (!push.stderr.is_empty()).then_some(push.stderr),
                    ));
                }
            }
        }
    }
    persist_receipt(&receipt_path, &receipt)?;

    if snapshot.default_branch != receipt.default_branch {
        let update = client.update_default_branch(&owner, &repo, &receipt.default_branch);
        snapshot = parse_repo_snapshot(&client.repo(&owner, &repo)?, &client, &owner, &repo)?;
        if snapshot.default_branch != receipt.default_branch {
            return Err(ForgeError::runtime_failure(
                error_schema(),
                "bootstrap_default_branch_failed",
                "Forgejo default branch update did not match read-back",
                update.err().map(|error| error.to_string()),
            ));
        }
        if update.is_err() {
            receipt.reconciled = true;
        }
    }
    receipt.default_branch_set = true;

    let final_branch =
        branch_sha(client.branch_optional(&owner, &repo, &receipt.default_branch)?)?.ok_or_else(
            || {
                ForgeError::runtime_failure(
                    error_schema(),
                    "bootstrap_readback_failed",
                    "Forgejo branch disappeared during final read-back",
                    None,
                )
            },
        )?;
    if final_branch != local_sha {
        return Err(remote_drift(&local_sha, &final_branch));
    }
    verify_provider_signature(&client.commit(&owner, &repo, &local_sha)?, &local_sha)?;

    receipt.complete = true;
    persist_receipt(&receipt_path, &receipt)?;
    let payload = BootstrapPayload {
        provider: provider.to_string(),
        repository,
        private: true,
        default_branch: receipt.default_branch.clone(),
        root_commit_sha: local_sha,
        signature_verified: true,
        remote_created: receipt.remote_created,
        pushed,
        reconciled: receipt.reconciled,
        idempotent: idempotent_candidate,
        checkout: checkout.display().to_string(),
        receipt: receipt_path.display().to_string(),
    };
    Ok(emit_success(
        schema_version_for(BINARY, "repo.bootstrap", 1),
        payload,
        format,
        |payload| {
            println!(
                "bootstrapped {} at {} on {} (signed: verified)",
                payload.repository, payload.root_commit_sha, payload.default_branch
            )
        },
    ))
}

fn prepare_files(paths: &[PathBuf]) -> Result<Vec<ReceiptFile>, ForgeError> {
    let mut files = Vec::with_capacity(paths.len());
    let mut total = 0u64;
    for path in paths {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            validation(
                "bootstrap_file_invalid",
                format!("failed to inspect --file '{}'", path.display()),
                Some(error.to_string()),
            )
        })?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err(validation(
                "bootstrap_file_invalid",
                "bootstrap inputs must be explicitly named regular files no larger than 4 MiB",
                Some(path.display().to_string()),
            ));
        }
        total = total.checked_add(metadata.len()).ok_or_else(|| {
            validation(
                "bootstrap_file_invalid",
                "bootstrap input size overflowed",
                None,
            )
        })?;
        if total > MAX_TOTAL_FILE_BYTES {
            return Err(validation(
                "bootstrap_file_invalid",
                "bootstrap inputs exceed the 16 MiB aggregate limit",
                None,
            ));
        }
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .filter(|name| valid_root_name(name))
            .ok_or_else(|| {
                validation(
                    "bootstrap_file_invalid",
                    "bootstrap file names must be safe UTF-8 repository-root names",
                    Some(path.display().to_string()),
                )
            })?
            .to_string();
        if files.iter().any(|file: &ReceiptFile| file.name == name) {
            return Err(validation(
                "bootstrap_file_invalid",
                format!("duplicate bootstrap root file name '{name}'"),
                None,
            ));
        }
        let bytes = fs::read(path).map_err(|error| {
            validation(
                "bootstrap_file_invalid",
                format!("failed to read --file '{}'", path.display()),
                Some(error.to_string()),
            )
        })?;
        files.push(ReceiptFile {
            source: fs::canonicalize(path)
                .map_err(|error| {
                    validation(
                        "bootstrap_file_invalid",
                        format!("failed to resolve --file '{}'", path.display()),
                        Some(error.to_string()),
                    )
                })?
                .display()
                .to_string(),
            name,
            sha256: sha256_hex(&bytes),
            bytes: metadata.len(),
        });
    }
    Ok(files)
}

fn create_signed_root(
    checkout: &Path,
    files: &[ReceiptFile],
    message: &str,
    branch: &str,
) -> Result<String, ForgeError> {
    create_private_dir(checkout)?;
    require_success(
        run_git(
            checkout,
            &["init", &format!("--initial-branch={branch}")],
            &[],
        )?,
        "bootstrap_git_init_failed",
        "failed to initialize the managed bootstrap checkout",
    )?;
    for file in files {
        let source = Path::new(&file.source);
        let bytes = fs::read(source).map_err(|error| {
            validation(
                "bootstrap_file_invalid",
                format!("failed to reread bootstrap file '{}'", source.display()),
                Some(error.to_string()),
            )
        })?;
        if sha256_hex(&bytes) != file.sha256 {
            return Err(validation(
                "bootstrap_file_changed",
                format!(
                    "bootstrap input '{}' changed after receipt creation",
                    source.display()
                ),
                None,
            ));
        }
        fs::write(checkout.join(&file.name), bytes).map_err(|error| {
            unavailable(
                "bootstrap_checkout_unavailable",
                "failed to populate the managed bootstrap checkout",
                Some(error.to_string()),
            )
        })?;
    }
    let mut add_args = vec![OsString::from("add"), OsString::from("--")];
    add_args.extend(files.iter().map(|file| OsString::from(&file.name)));
    require_success(
        run_git_os(checkout, &add_args, &[])?,
        "bootstrap_git_add_failed",
        "failed to stage the explicit bootstrap files",
    )?;

    author_signed_root(checkout, message)
}

fn author_signed_root(checkout: &Path, message: &str) -> Result<String, ForgeError> {
    let semantic = semantic_commit_bin();
    let semantic_args = [
        OsString::from("commit"),
        OsString::from("--repo"),
        checkout.as_os_str().to_os_string(),
        OsString::from("--message"),
        OsString::from(message),
        OsString::from("--automation"),
        OsString::from("--json"),
        OsString::from("--no-summary"),
        OsString::from("--quiet"),
    ];
    require_success(
        run_process(&semantic, None, &semantic_args, &[])?,
        "bootstrap_commit_failed",
        "semantic-commit failed to author the bootstrap root commit",
    )?;

    let sha = require_success(
        run_git(checkout, &["rev-parse", "--verify", "HEAD^{commit}"], &[])?,
        "bootstrap_commit_missing",
        "failed to resolve the bootstrap root commit",
    )?
    .stdout
    .trim()
    .to_string();
    validate_oid(&sha)?;
    let parents = require_success(
        run_git(checkout, &["rev-list", "--parents", "-n", "1", &sha], &[])?,
        "bootstrap_commit_invalid",
        "failed to verify the bootstrap commit ancestry",
    )?;
    if parents.stdout.split_whitespace().collect::<Vec<_>>() != [sha.as_str()] {
        return Err(validation(
            "bootstrap_commit_not_root",
            "bootstrap commit must have zero parents",
            None,
        ));
    }
    let signature = require_success(
        run_git(checkout, &["log", "-1", "--format=%G?", &sha], &[])?,
        "bootstrap_signature_check_failed",
        "failed to verify the bootstrap commit signature",
    )?;
    if signature.stdout.trim() != "G" {
        return Err(validation(
            "commit_signature_unverified",
            format!(
                "bootstrap root commit must have a locally verified good signature; git reported '{}'",
                signature.stdout.trim()
            ),
            None,
        ));
    }
    Ok(sha)
}

fn recover_signed_root(
    checkout: &Path,
    files: &[ReceiptFile],
    message: &str,
    branch: &str,
) -> Result<Option<String>, ForgeError> {
    let metadata = match fs::symlink_metadata(checkout) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(unavailable(
                "bootstrap_checkout_unavailable",
                "failed to inspect the managed bootstrap checkout",
                Some(error.to_string()),
            ));
        }
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(checkout_mismatch(
            "managed bootstrap checkout is not a real directory",
        ));
    }

    let observed_branch = require_success(
        run_git(
            checkout,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            &[],
        )?,
        "bootstrap_checkout_mismatch",
        "managed bootstrap checkout is not on the expected branch",
    )?;
    if observed_branch.stdout.trim() != branch {
        return Err(checkout_mismatch(
            "managed bootstrap checkout branch does not match the receipt",
        ));
    }
    validate_checkout_files(checkout, files)?;

    let head = run_git(checkout, &["rev-parse", "--verify", "HEAD^{commit}"], &[])?;
    if !head.success {
        validate_staged_partial_checkout(checkout, files)?;
        return author_signed_root(checkout, message).map(Some);
    }
    let sha = head.stdout.trim().to_string();
    if validate_oid(&sha).is_err() {
        return Err(checkout_mismatch(
            "managed bootstrap checkout HEAD is not a valid commit object id",
        ));
    }
    validate_committed_checkout(checkout, files)?;

    let parents = require_success(
        run_git(checkout, &["rev-list", "--parents", "-n", "1", &sha], &[])?,
        "bootstrap_checkout_mismatch",
        "failed to verify recovered bootstrap commit ancestry",
    )?;
    if parents.stdout.split_whitespace().collect::<Vec<_>>() != [sha.as_str()] {
        return Err(checkout_mismatch(
            "managed bootstrap checkout HEAD is not a zero-parent commit",
        ));
    }
    let signature = require_success(
        run_git(checkout, &["log", "-1", "--format=%G?", &sha], &[])?,
        "bootstrap_signature_check_failed",
        "failed to verify the recovered bootstrap commit signature",
    )?;
    if signature.stdout.trim() != "G" {
        return Err(validation(
            "commit_signature_unverified",
            format!(
                "recovered bootstrap root commit must have a locally verified good signature; git reported '{}'",
                signature.stdout.trim()
            ),
            None,
        ));
    }
    Ok(Some(sha))
}

fn validate_staged_partial_checkout(
    checkout: &Path,
    files: &[ReceiptFile],
) -> Result<(), ForgeError> {
    let status = run_git(
        checkout,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        &[],
    )?;
    if !status.success {
        return Err(checkout_mismatch(
            "failed to inspect the no-HEAD managed bootstrap checkout",
        ));
    }
    let mut observed = nul_entries(&status.stdout);
    observed.sort();
    let mut expected = files
        .iter()
        .map(|file| format!("A  {}", file.name))
        .collect::<Vec<_>>();
    expected.sort();
    if observed != expected {
        return Err(checkout_mismatch(
            "no-HEAD managed bootstrap checkout is not an exact safely staged state",
        ));
    }
    Ok(())
}

fn validate_committed_checkout(checkout: &Path, files: &[ReceiptFile]) -> Result<(), ForgeError> {
    let status = run_git(
        checkout,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        &[],
    )?;
    if !status.success || !status.stdout.is_empty() {
        return Err(checkout_mismatch(
            "managed bootstrap checkout HEAD, index, and working tree are not clean",
        ));
    }

    let tree = run_git(
        checkout,
        &["ls-tree", "-r", "--name-only", "-z", "HEAD"],
        &[],
    )?;
    if !tree.success {
        return Err(checkout_mismatch(
            "failed to inspect the managed bootstrap checkout HEAD tree",
        ));
    }
    let mut observed = nul_entries(&tree.stdout);
    observed.sort();
    let mut expected = files
        .iter()
        .map(|file| file.name.clone())
        .collect::<Vec<_>>();
    expected.sort();
    if observed != expected {
        return Err(checkout_mismatch(
            "managed bootstrap checkout HEAD tree does not exactly match the receipt",
        ));
    }
    Ok(())
}

fn nul_entries(value: &str) -> Vec<String> {
    value
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate_checkout_files(checkout: &Path, files: &[ReceiptFile]) -> Result<(), ForgeError> {
    let mut observed_names = Vec::new();
    let entries = fs::read_dir(checkout).map_err(|error| {
        unavailable(
            "bootstrap_checkout_unavailable",
            "failed to enumerate the managed bootstrap checkout",
            Some(error.to_string()),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            unavailable(
                "bootstrap_checkout_unavailable",
                "failed to enumerate the managed bootstrap checkout",
                Some(error.to_string()),
            )
        })?;
        let name = entry.file_name().into_string().map_err(|_| {
            checkout_mismatch("managed bootstrap checkout contains a non-UTF-8 root entry")
        })?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            unavailable(
                "bootstrap_checkout_unavailable",
                "failed to inspect a managed bootstrap checkout entry",
                Some(error.to_string()),
            )
        })?;
        if name == ".git" {
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                return Err(checkout_mismatch(
                    "managed bootstrap checkout has an unsafe .git entry",
                ));
            }
            continue;
        }
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(checkout_mismatch(
                "managed bootstrap checkout contains an unexpected non-regular entry",
            ));
        }
        observed_names.push(name);
    }

    observed_names.sort();
    let mut expected_names = files
        .iter()
        .map(|file| file.name.clone())
        .collect::<Vec<_>>();
    expected_names.sort();
    if observed_names != expected_names {
        return Err(checkout_mismatch(
            "managed bootstrap checkout files do not exactly match the receipt",
        ));
    }
    for file in files {
        let path = checkout.join(&file.name);
        let bytes = fs::read(&path).map_err(|error| {
            unavailable(
                "bootstrap_checkout_unavailable",
                "failed to read a managed bootstrap checkout file",
                Some(error.to_string()),
            )
        })?;
        if bytes.len() as u64 != file.bytes || sha256_hex(&bytes) != file.sha256 {
            return Err(checkout_mismatch(
                "managed bootstrap checkout file content does not match the receipt",
            ));
        }
    }
    Ok(())
}

fn checkout_mismatch(message: &'static str) -> ForgeError {
    validation("bootstrap_checkout_mismatch", message, None)
}

fn push_once(
    checkout: &Path,
    clone_url: &str,
    branch: &str,
    sha: &str,
    askpass: &Path,
    token_env: &str,
    username: &str,
) -> Result<ProcessResult, ForgeError> {
    let refspec = format!("{sha}:refs/heads/{branch}");
    let args = [
        "-c",
        "http.followRedirects=false",
        "-c",
        "push.followTags=false",
        "-c",
        "push.pushOption=",
        "-c",
        "push.recurseSubmodules=no",
        "push",
        "--porcelain",
        "--no-follow-tags",
        "--no-recurse-submodules",
        "--no-push-option",
        "--",
        clone_url,
        &refspec,
    ];
    let env = [
        (
            OsString::from("GIT_ASKPASS"),
            askpass.as_os_str().to_os_string(),
        ),
        (OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0")),
        (OsString::from("GCM_INTERACTIVE"), OsString::from("never")),
        (
            OsString::from("FORGE_CLI_BOOTSTRAP_TOKEN_ENV"),
            OsString::from(token_env),
        ),
        (
            OsString::from("FORGE_CLI_BOOTSTRAP_USERNAME"),
            OsString::from(username),
        ),
    ];
    run_git(checkout, &args, &env)
}

fn write_askpass(state_dir: &Path) -> Result<PathBuf, ForgeError> {
    let path = state_dir.join("git-askpass.sh");
    let body = b"#!/bin/sh\ncase \"$1\" in\n  *Username*) printf '%s\\n' \"$FORGE_CLI_BOOTSTRAP_USERNAME\" ;;\n  *Password*) printenv \"$FORGE_CLI_BOOTSTRAP_TOKEN_ENV\" ;;\n  *) exit 1 ;;\nesac\n";
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(0o700);
    let mut file = options.open(&path).map_err(|error| {
        unavailable(
            "bootstrap_checkout_unavailable",
            "failed to create the managed Git credential helper",
            Some(error.to_string()),
        )
    })?;
    file.write_all(body)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            unavailable(
                "bootstrap_checkout_unavailable",
                "failed to persist the managed Git credential helper",
                Some(error.to_string()),
            )
        })?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        unavailable(
            "bootstrap_checkout_unavailable",
            "failed to secure the managed Git credential helper",
            Some(error.to_string()),
        )
    })?;
    Ok(path)
}

fn parse_repo_snapshot(
    value: &serde_json::Value,
    client: &ForgejoClient,
    owner: &str,
    repo: &str,
) -> Result<RepoSnapshot, ForgeError> {
    let observed_owner = value
        .pointer("/owner/login")
        .and_then(serde_json::Value::as_str);
    let observed_name = value.get("name").and_then(serde_json::Value::as_str);
    let private = value.get("private").and_then(serde_json::Value::as_bool);
    if observed_owner != Some(owner) || observed_name != Some(repo) || private != Some(true) {
        return Err(validation(
            "remote_drift",
            "Forgejo repository read-back does not match the requested private repository",
            None,
        ));
    }
    let clone_url = value
        .get("clone_url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| software("Forgejo repository read-back omitted clone_url"))?;
    Ok(RepoSnapshot {
        clone_url: client.same_origin_clone_url(clone_url)?,
        default_branch: value
            .get("default_branch")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        empty: value
            .get("empty")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| software("Forgejo repository read-back omitted empty"))?,
    })
}

fn branch_sha(value: Option<serde_json::Value>) -> Result<Option<String>, ForgeError> {
    value
        .map(|value| {
            value
                .pointer("/commit/id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| software("Forgejo branch read-back omitted commit.id"))
        })
        .transpose()
}

fn verify_provider_signature(value: &serde_json::Value, expected: &str) -> Result<(), ForgeError> {
    let observed = value.get("sha").and_then(serde_json::Value::as_str);
    let verified = value
        .pointer("/commit/verification/verified")
        .and_then(serde_json::Value::as_bool);
    if observed != Some(expected) {
        return Err(remote_drift(expected, observed.unwrap_or("<missing>")));
    }
    if verified != Some(true) {
        return Err(validation(
            "provider_signature_unverified",
            "Forgejo did not verify the delivered root commit signature",
            None,
        ));
    }
    Ok(())
}

fn validate_receipt_inputs(
    actual: &BootstrapReceipt,
    expected: &BootstrapReceipt,
) -> Result<(), ForgeError> {
    if actual.schema_version != RECEIPT_SCHEMA
        || actual.provider != expected.provider
        || actual.repository != expected.repository
        || actual.owner_kind != expected.owner_kind
        || actual.default_branch != expected.default_branch
        || actual.message != expected.message
        || actual.reason != expected.reason
        || actual.files != expected.files
    {
        return Err(validation(
            "bootstrap_receipt_mismatch",
            "existing bootstrap receipt does not match the exact requested inputs",
            None,
        ));
    }
    Ok(())
}

fn bootstrap_state_dir(provider: &str, owner: &str, repo: &str) -> Result<PathBuf, ForgeError> {
    let root = match std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    unavailable(
                        "bootstrap_state_unavailable",
                        "cannot resolve bootstrap state without XDG_STATE_HOME or HOME",
                        None,
                    )
                })?,
        )
        .join(".local/state"),
    };
    let dir = root
        .join("forge-cli/repo-bootstrap")
        .join(provider)
        .join(owner)
        .join(repo);
    create_private_dir(&dir)?;
    Ok(dir)
}

fn create_private_dir(path: &Path) -> Result<(), ForgeError> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (!metadata.file_type().is_dir() || metadata.file_type().is_symlink())
    {
        return Err(validation(
            "bootstrap_state_unsafe",
            "bootstrap state path must be a real directory",
            Some(path.display().to_string()),
        ));
    }
    fs::create_dir_all(path).map_err(|error| {
        unavailable(
            "bootstrap_state_unavailable",
            format!(
                "failed to create bootstrap state '{}': {error}",
                path.display()
            ),
            None,
        )
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        unavailable(
            "bootstrap_state_unavailable",
            format!(
                "failed to secure bootstrap state '{}': {error}",
                path.display()
            ),
            None,
        )
    })?;
    Ok(())
}

fn load_receipt(path: &Path) -> Result<Option<BootstrapReceipt>, ForgeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(unavailable(
                "bootstrap_receipt_unavailable",
                "failed to inspect the bootstrap receipt",
                Some(error.to_string()),
            ));
        }
    };
    if !metadata.file_type().is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > MAX_RECEIPT_BYTES as u64
    {
        return Err(validation(
            "bootstrap_receipt_unsafe",
            "bootstrap receipt must be a private bounded regular file",
            Some(path.display().to_string()),
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| {
            file.take((MAX_RECEIPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| {
            unavailable(
                "bootstrap_receipt_unavailable",
                "failed to read the bootstrap receipt",
                Some(error.to_string()),
            )
        })?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(validation(
            "bootstrap_receipt_unsafe",
            "bootstrap receipt exceeds its size bound",
            None,
        ));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        validation(
            "bootstrap_receipt_invalid",
            "bootstrap receipt is not valid JSON",
            Some(error.to_string()),
        )
    })
}

fn persist_receipt(path: &Path, receipt: &BootstrapReceipt) -> Result<(), ForgeError> {
    let bytes = receipt_bytes(receipt)?;
    let parent = path
        .parent()
        .ok_or_else(|| software("bootstrap receipt has no parent"))?;
    create_private_dir(parent)?;
    let temp = parent.join(format!(".receipt.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&temp).map_err(|error| {
        unavailable(
            "bootstrap_receipt_unavailable",
            "failed to create the bootstrap receipt temporary file",
            Some(error.to_string()),
        )
    })?;
    let result = file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&temp, path));
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(unavailable(
            "bootstrap_receipt_unavailable",
            "failed to persist the bootstrap receipt",
            Some(error.to_string()),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        unavailable(
            "bootstrap_receipt_unavailable",
            "failed to secure the bootstrap receipt",
            Some(error.to_string()),
        )
    })?;
    File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            unavailable(
                "bootstrap_receipt_unavailable",
                "failed to sync the bootstrap receipt directory",
                Some(error.to_string()),
            )
        })?;
    Ok(())
}

fn create_receipt(path: &Path, receipt: &BootstrapReceipt) -> Result<(), ForgeError> {
    let bytes = receipt_bytes(receipt)?;
    let parent = path
        .parent()
        .ok_or_else(|| software("bootstrap receipt has no parent"))?;
    create_private_dir(parent)?;
    let temp = parent.join(format!(".receipt.{}.create", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&temp).map_err(|error| {
        unavailable(
            "bootstrap_receipt_unavailable",
            "failed to create the bootstrap receipt candidate",
            Some(error.to_string()),
        )
    })?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(unavailable(
            "bootstrap_receipt_unavailable",
            "failed to persist the bootstrap receipt candidate",
            Some(error.to_string()),
        ));
    }
    let linked = fs::hard_link(&temp, path);
    let _ = fs::remove_file(&temp);
    match linked {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(validation(
                "bootstrap_resume_required",
                "another bootstrap invocation created the durable receipt; inspect it and pass --resume",
                Some(path.display().to_string()),
            ));
        }
        Err(error) => {
            return Err(unavailable(
                "bootstrap_receipt_unavailable",
                "failed to install the bootstrap receipt atomically",
                Some(error.to_string()),
            ));
        }
    }
    File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            unavailable(
                "bootstrap_receipt_unavailable",
                "failed to sync the bootstrap receipt directory",
                Some(error.to_string()),
            )
        })?;
    Ok(())
}

fn receipt_bytes(receipt: &BootstrapReceipt) -> Result<Vec<u8>, ForgeError> {
    let bytes = serde_json::to_vec_pretty(receipt).map_err(|error| {
        software_with_detail(
            "failed to serialize the bootstrap receipt",
            error.to_string(),
        )
    })?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(software(
            "serialized bootstrap receipt exceeds its size bound",
        ));
    }
    Ok(bytes)
}

fn run_git(
    cwd: &Path,
    args: &[&str],
    env: &[(OsString, OsString)],
) -> Result<ProcessResult, ForgeError> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_git_os(cwd, &args, env)
}

fn run_git_os(
    cwd: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
) -> Result<ProcessResult, ForgeError> {
    let mut all = vec![OsString::from("-C"), cwd.as_os_str().to_os_string()];
    all.extend_from_slice(args);
    run_process(&git_bin(), None, &all, env)
}

fn run_process(
    executable: &OsStr,
    cwd: Option<&Path>,
    args: &[OsString],
    env: &[(OsString, OsString)],
) -> Result<ProcessResult, ForgeError> {
    let mut command = Command::new(executable);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let output = output_with_limits(&mut command, Some(PROCESS_TIMEOUT), PROCESS_OUTPUT_LIMIT)
        .map_err(|error| match error {
            ProcessOutputError::Io(error) => unavailable(
                "bootstrap_process_unavailable",
                format!(
                    "failed to launch '{}': {error}",
                    executable.to_string_lossy()
                ),
                None,
            ),
            ProcessOutputError::Timeout { .. } => unavailable(
                "bootstrap_process_timeout",
                format!(
                    "'{}' exceeded the 120 second deadline",
                    executable.to_string_lossy()
                ),
                None,
            ),
            ProcessOutputError::OutputLimit { .. } => unavailable(
                "bootstrap_process_output_limit",
                format!(
                    "'{}' exceeded the output bound",
                    executable.to_string_lossy()
                ),
                None,
            ),
        })?;
    Ok(ProcessResult {
        success: output.status.success(),
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: redact_and_tail(&String::from_utf8_lossy(&output.stderr)),
    })
}

fn require_success(
    result: ProcessResult,
    kind: &'static str,
    message: &'static str,
) -> Result<ProcessResult, ForgeError> {
    if result.success {
        Ok(result)
    } else {
        Err(ForgeError::runtime_failure(
            error_schema(),
            kind,
            message,
            Some(format!("exit={}; {}", result.code, result.stderr)),
        ))
    }
}

fn git_bin() -> OsString {
    if cfg!(debug_assertions) {
        std::env::var_os(GIT_BIN_ENV)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("git"))
    } else {
        OsString::from("git")
    }
}

fn semantic_commit_bin() -> OsString {
    if cfg!(debug_assertions) {
        std::env::var_os(SEMANTIC_COMMIT_BIN_ENV)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("semantic-commit"))
    } else {
        OsString::from("semantic-commit")
    }
}

fn read_small_regular_file(path: &Path, max: usize, label: &str) -> Result<String, ForgeError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        validation(
            "bootstrap_authorization_invalid",
            format!("failed to inspect {label} file '{}'", path.display()),
            Some(error.to_string()),
        )
    })?;
    if !metadata.file_type().is_file() || metadata.len() > max as u64 {
        return Err(validation(
            "bootstrap_authorization_invalid",
            format!("{label} must be a bounded regular file"),
            None,
        ));
    }
    let text = fs::read_to_string(path).map_err(|error| {
        validation(
            "bootstrap_authorization_invalid",
            format!("failed to read {label} file '{}'", path.display()),
            Some(error.to_string()),
        )
    })?;
    let text = text.trim().to_string();
    if text.is_empty()
        || text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err(validation(
            "bootstrap_authorization_invalid",
            format!("{label} must contain non-empty UTF-8 text without control bytes"),
            None,
        ));
    }
    Ok(text)
}

fn validate_branch(branch: &str) -> Result<(), ForgeError> {
    let valid = !branch.is_empty()
        && branch.len() <= 255
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.ends_with('.')
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch
            .split('/')
            .any(|part| part.is_empty() || part == "." || part.ends_with(".lock"))
        && branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'));
    if valid {
        Ok(())
    } else {
        Err(validation(
            "bootstrap_default_branch_invalid",
            "bootstrap default branch is not a safe Git branch name",
            None,
        ))
    }
}

fn validate_message(message: &str) -> Result<(), ForgeError> {
    if message.trim().is_empty() || message.len() > 10_000 || message.contains('\0') {
        Err(validation(
            "bootstrap_message_invalid",
            "bootstrap commit message must be non-empty UTF-8 text no larger than 10000 bytes",
            None,
        ))
    } else {
        Ok(())
    }
}

fn validate_oid(value: &str) -> Result<(), ForgeError> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(software(
            "Git returned an invalid bootstrap commit object id",
        ))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn valid_root_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | ".." | ".git")
        && !name.contains('/')
        && !name.contains('\0')
}

fn remote_drift(expected: &str, observed: &str) -> ForgeError {
    validation(
        "remote_drift",
        format!(
            "remote branch differs from the durable root commit: expected {expected}, observed {observed}"
        ),
        None,
    )
}

fn validation(
    kind: &'static str,
    message: impl Into<String>,
    detail: Option<String>,
) -> ForgeError {
    ForgeError::validation(error_schema(), kind, message, detail)
}

fn unavailable(
    kind: &'static str,
    message: impl Into<String>,
    detail: Option<String>,
) -> ForgeError {
    ForgeError::unavailable(error_schema(), kind, message, detail)
}

fn software(message: impl Into<String>) -> ForgeError {
    ForgeError::software(error_schema(), message, None)
}

fn software_with_detail(message: impl Into<String>, detail: String) -> ForgeError {
    ForgeError::software(error_schema(), message, Some(detail))
}

fn error_schema() -> String {
    schema_version_for(BINARY, "error", 1)
}
