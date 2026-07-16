//! Governed direct-default-branch delivery.
//!
//! `repo push-default` is the narrow exception to forge-cli's PR-first
//! delivery policy. It never authors a commit and never exposes a force mode.
//! The caller supplies the exact remote base observed before authoring one
//! signed commit in a non-default managed worktree. This op rechecks that
//! contract, performs one expected-old-OID compare-and-swap fast-forward, and
//! reads the remote ref back.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use serde::Serialize;

use crate::backend::{
    BackendRunner, ProcessOutputError, format_duration, output_with_limits, redact_and_tail,
};
use crate::cli::{BINARY, GlobalFlags, RepoPushDefaultArgs};
use crate::envelope::emit_success;
use crate::error::ForgeError;
use crate::ops::repo_view;
use crate::provider::{Provider, classify_host, detect, parse_host, parse_slug};
use crate::rate_limit::default_runner;
use crate::validations::no_local_path;

const SCHEMA: &str = "repo.push-default";
const SCHEMA_VERSION: u32 = 1;
const MAX_REASON_BYTES: usize = 2_000;
const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_GIT_CAPTURE_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// Testing-only executable override for local Git subprocesses.
pub const ENV_GIT_BIN: &str = "FORGE_CLI_GIT_BIN";
/// Testing-only timeout override for local Git subprocesses.
pub const ENV_GIT_TIMEOUT_MS: &str = "FORGE_CLI_GIT_TIMEOUT_MS";
/// Testing-only capture-limit override for local Git subprocesses.
pub const ENV_GIT_CAPTURE_LIMIT_BYTES: &str = "FORGE_CLI_GIT_CAPTURE_LIMIT_BYTES";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait GitRunner {
    fn run(&self, workdir: &Path, args: &[OsString]) -> Result<GitOutput, ForgeError>;
}

#[derive(Debug, Default)]
struct ProcessGitRunner;

impl GitRunner for ProcessGitRunner {
    fn run(&self, workdir: &Path, args: &[OsString]) -> Result<GitOutput, ForgeError> {
        let executable = std::env::var_os(ENV_GIT_BIN)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("git"));
        let mut command = Command::new(&executable);
        command.arg("-C").arg(workdir).args(args);
        let timeout = configured_git_timeout();
        let capture_limit = configured_git_capture_limit();
        let output = match output_with_limits(&mut command, Some(timeout), capture_limit) {
            Ok(output) => output,
            Err(ProcessOutputError::Io(error)) => {
                return Err(ForgeError::software(
                    schema_error(),
                    format!("failed to launch {}", executable.to_string_lossy()),
                    Some(error.to_string()),
                ));
            }
            Err(ProcessOutputError::Timeout { timeout, output }) => {
                let stderr = redact_and_tail(&String::from_utf8_lossy(&output.stderr));
                return Err(ForgeError::unavailable(
                    schema_error(),
                    "git_timeout",
                    format!(
                        "{} timed out after {}",
                        executable.to_string_lossy(),
                        format_duration(timeout)
                    ),
                    (!stderr.is_empty()).then_some(stderr),
                ));
            }
            Err(ProcessOutputError::OutputLimit {
                stream,
                limit,
                output,
            }) => {
                let stderr = redact_and_tail(&String::from_utf8_lossy(&output.stderr));
                return Err(ForgeError::unavailable(
                    schema_error(),
                    "git_output_limit",
                    format!(
                        "{} {} exceeded the {}-byte capture limit",
                        executable.to_string_lossy(),
                        stream.as_str(),
                        limit
                    ),
                    (!stderr.is_empty()).then_some(stderr),
                ));
            }
        };
        Ok(GitOutput {
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: redact_and_tail(&String::from_utf8_lossy(&output.stderr)),
        })
    }
}

fn configured_git_timeout() -> Duration {
    std::env::var(ENV_GIT_TIMEOUT_MS)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_GIT_TIMEOUT)
}

fn configured_git_capture_limit() -> usize {
    std::env::var(ENV_GIT_CAPTURE_LIMIT_BYTES)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_GIT_CAPTURE_LIMIT_BYTES)
}

/// Receipt emitted after validation and optional delivery.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoPushDefaultPayload {
    pub provider: &'static str,
    pub repository: String,
    pub remote: String,
    pub default_branch: String,
    pub authoring_branch: String,
    pub head: String,
    pub head_sha: String,
    pub expected_base: String,
    pub reason: String,
    pub push_refspec: String,
    pub pushed: bool,
    pub observed_remote_sha: String,
}

/// Production entry point.
pub fn run(
    global: &GlobalFlags,
    args: RepoPushDefaultArgs,
    format: OutputFormat,
) -> Result<i32, ForgeError> {
    let backend = default_runner();
    let git = ProcessGitRunner;
    let workdir = std::env::current_dir().map_err(|error| {
        ForgeError::software(
            schema_error(),
            "failed to resolve the current working directory",
            Some(error.to_string()),
        )
    })?;
    run_with(&backend, &git, global, args, format, &workdir)
}

pub fn run_with<R: BackendRunner, G: GitRunner>(
    backend: &R,
    git: &G,
    global: &GlobalFlags,
    args: RepoPushDefaultArgs,
    format: OutputFormat,
    workdir: &Path,
) -> Result<i32, ForgeError> {
    let expected_base = validate_object_id(&args.expected_base)?;
    let reason = read_reason(&args.reason_file)?;

    let push_url_lookup = git.run(
        workdir,
        &os_args(&[
            "remote",
            "get-url",
            "--push",
            "--all",
            "--",
            global.remote.as_str(),
        ]),
    )?;
    if !push_url_lookup.success {
        return Err(validation(
            "push_destination_missing",
            format!(
                "failed to resolve the push destination for Git remote '{}'",
                global.remote
            ),
            (!push_url_lookup.stderr.is_empty()).then_some(push_url_lookup.stderr),
        ));
    }
    let push_url = unique_push_url(&push_url_lookup.stdout, &global.remote)?;
    reject_http_push_url_userinfo(&push_url)?;

    let ctx = detect(
        global.provider_hint(),
        &global.remote,
        global.repo.as_deref(),
        |_| Some(push_url.clone()),
    )?;
    bind_remote_provider(&push_url, ctx.provider)?;
    let remote_slug = parse_slug(&push_url).ok_or_else(|| {
        validation(
            "repository_mismatch",
            "the selected Git push URL does not identify an owner/repository slug",
            None,
        )
    })?;
    if let Some(explicit_repo) = global.repo.as_deref()
        && !explicit_repo.eq_ignore_ascii_case(&remote_slug)
    {
        return Err(validation(
            "repository_mismatch",
            format!(
                "explicit repository '{explicit_repo}' does not match push destination '{remote_slug}'"
            ),
            None,
        ));
    }
    let mut metadata_ctx = ctx.clone();
    metadata_ctx.repo = Some(metadata_repo_locator(ctx.provider, &ctx.host, &remote_slug));
    let repo = repo_view::compute(backend, &metadata_ctx)?;
    let repository = format!("{}/{}", repo.owner, repo.name);
    bind_remote_repository(&push_url, &repository)?;
    bind_remote_metadata(&push_url, &repo.url, &repository)?;

    let default_ref = format!("refs/heads/{}", repo.default_branch);
    let default_ref_check = git.run(workdir, &os_args(&["check-ref-format", &default_ref]))?;
    if !default_ref_check.success {
        return Err(ForgeError::software(
            schema_error(),
            "provider returned an invalid default branch ref",
            None,
        ));
    }

    let status = git_capture(
        git,
        workdir,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
        "worktree status",
    )?;
    if !status.trim().is_empty() {
        return Err(validation(
            "dirty_worktree",
            "repo push-default requires a clean worktree",
            None,
        ));
    }

    let branch_lookup = git.run(
        workdir,
        &os_args(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
    )?;
    if !branch_lookup.success {
        return Err(validation(
            "detached_head",
            "repo push-default requires a checked-out non-default branch",
            None,
        ));
    }
    let branch = branch_lookup.stdout.trim().to_string();
    if branch.is_empty() {
        return Err(validation(
            "detached_head",
            "repo push-default requires a checked-out non-default branch",
            None,
        ));
    }
    if branch == repo.default_branch {
        return Err(validation(
            "default_branch_checkout",
            format!(
                "refusing to author direct delivery from the checked-out default branch '{}'",
                repo.default_branch
            ),
            Some("create a managed worktree from the remote default branch".into()),
        ));
    }
    let head_sha = git_capture(
        git,
        workdir,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        "HEAD resolution",
    )?;
    let head_sha = validate_object_id(head_sha.trim())?;
    let requested_head = format!("{}^{{commit}}", args.head);
    let requested_head_lookup = git.run(
        workdir,
        &os_args(&["rev-parse", "--verify", "--end-of-options", &requested_head]),
    )?;
    if !requested_head_lookup.success {
        return Err(validation(
            "head_not_checked_out",
            format!("--head '{}' does not resolve to a commit", args.head),
            (!requested_head_lookup.stderr.is_empty()).then_some(requested_head_lookup.stderr),
        ));
    }
    let requested_head_sha = validate_object_id(requested_head_lookup.stdout.trim())?;
    if requested_head_sha != head_sha {
        return Err(validation(
            "head_not_checked_out",
            format!(
                "--head '{}' resolves to {requested_head_sha}, not checked-out HEAD {head_sha}",
                args.head
            ),
            Some(format!("checked-out branch: {branch}")),
        ));
    }

    let observed_base = ls_remote_head(git, workdir, &push_url, &global.remote, &default_ref)?;
    if observed_base != expected_base {
        return Err(validation(
            "expected_base_mismatch",
            format!(
                "remote default branch moved: expected {expected_base}, observed {observed_base}"
            ),
            Some("recreate the managed worktree from the current remote default branch".into()),
        ));
    }

    let expected_commit = format!("{expected_base}^{{commit}}");
    let expected_object = git.run(workdir, &os_args(&["cat-file", "-e", &expected_commit]))?;
    if !expected_object.success {
        return Err(validation(
            "expected_base_missing",
            "the expected remote base commit is not available locally",
            Some("fetch the selected remote and recreate the managed worktree".into()),
        ));
    }
    let ancestry = git.run(
        workdir,
        &os_args(&["merge-base", "--is-ancestor", &expected_base, &head_sha]),
    )?;
    if !ancestry.success && ancestry.exit_code == 1 {
        return Err(validation(
            "expected_base_not_ancestor",
            "the expected remote base is not an ancestor of HEAD",
            None,
        ));
    }
    if !ancestry.success {
        return Err(ForgeError::software(
            schema_error(),
            "git ancestry check failed",
            (!ancestry.stderr.is_empty()).then_some(ancestry.stderr),
        ));
    }

    let range = format!("{expected_base}..{head_sha}");
    let count = git_capture(
        git,
        workdir,
        &["rev-list", "--count", &range],
        "commit count",
    )?;
    let count = count.trim().parse::<u64>().map_err(|error| {
        ForgeError::software(
            schema_error(),
            "git rev-list returned a non-numeric commit count",
            Some(error.to_string()),
        )
    })?;
    if count != 1 {
        return Err(validation(
            "direct_commit_count_invalid",
            format!("repo push-default requires exactly one commit ahead; observed {count}"),
            None,
        ));
    }

    let signature = git_capture(
        git,
        workdir,
        &["log", "-1", "--format=%G?", &head_sha],
        "commit signature verification",
    )?;
    if signature.trim() != "G" {
        return Err(validation(
            "commit_signature_unverified",
            format!(
                "HEAD must have a locally verified good signature; git reported '{}'",
                signature.trim()
            ),
            Some("commit through semantic-commit with signing enabled".into()),
        ));
    }

    let push_refspec = format!("{head_sha}:{default_ref}");
    if global.dry_run {
        let payload = RepoPushDefaultPayload {
            provider: ctx.provider.as_str(),
            repository,
            remote: global.remote.clone(),
            default_branch: repo.default_branch,
            authoring_branch: branch,
            head: args.head,
            head_sha,
            expected_base: expected_base.clone(),
            reason,
            push_refspec,
            pushed: false,
            observed_remote_sha: expected_base,
        };
        return Ok(emit_success(schema_success(), payload, format, render_text));
    }

    let exact_lease = format!("--force-with-lease={default_ref}:{expected_base}");
    let push = git.run(
        workdir,
        &os_args(&[
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
            &exact_lease,
            "--",
            &push_url,
            &push_refspec,
        ]),
    )?;
    if !push.success {
        return Err(ForgeError::runtime_failure(
            schema_error(),
            "default_push_rejected",
            "the compare-and-swap fast-forward push to the remote default branch was rejected",
            (!push.stderr.is_empty()).then_some(push.stderr),
        ));
    }

    let observed_remote_sha =
        ls_remote_head(git, workdir, &push_url, &global.remote, &default_ref)?;
    if observed_remote_sha != head_sha {
        return Err(ForgeError::runtime_failure(
            schema_error(),
            "default_push_verification_failed",
            format!(
                "post-push remote read-back did not match delivered HEAD: expected {head_sha}, observed {observed_remote_sha}"
            ),
            Some(
                "the remote may have changed after delivery; inspect it before any further action"
                    .into(),
            ),
        ));
    }

    let payload = RepoPushDefaultPayload {
        provider: ctx.provider.as_str(),
        repository,
        remote: global.remote.clone(),
        default_branch: repo.default_branch,
        authoring_branch: branch,
        head: args.head,
        head_sha,
        expected_base,
        reason,
        push_refspec,
        pushed: true,
        observed_remote_sha,
    };
    Ok(emit_success(schema_success(), payload, format, render_text))
}

fn read_reason(path: &Path) -> Result<String, ForgeError> {
    let bytes = fs::read(path).map_err(|error| {
        validation(
            "reason_file_unreadable",
            format!("failed to read --reason-file '{}'", path.display()),
            Some(error.to_string()),
        )
    })?;
    if bytes.len() > MAX_REASON_BYTES {
        return Err(validation(
            "reason_invalid",
            format!("direct-main reason exceeds {MAX_REASON_BYTES} bytes"),
            None,
        ));
    }
    let raw = String::from_utf8(bytes).map_err(|error| {
        validation(
            "reason_invalid",
            "direct-main reason must be valid UTF-8",
            Some(error.to_string()),
        )
    })?;
    let reason = raw.trim().to_string();
    if reason.is_empty() {
        return Err(validation(
            "reason_invalid",
            "direct-main reason must not be empty",
            None,
        ));
    }
    if reason
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err(validation(
            "reason_invalid",
            "direct-main reason contains unsupported control characters",
            None,
        ));
    }
    no_local_path(&reason, "direct-main reason")?;
    Ok(reason)
}

fn unique_push_url(output: &str, remote: &str) -> Result<String, ForgeError> {
    let destinations: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match destinations.as_slice() {
        [destination] => Ok((*destination).to_string()),
        [] => Err(validation(
            "push_destination_missing",
            format!("git remote '{remote}' has no push destination"),
            None,
        )),
        _ => Err(validation(
            "push_destination_ambiguous",
            format!(
                "git remote '{remote}' has multiple push destinations; direct-main delivery requires exactly one"
            ),
            None,
        )),
    }
}

fn reject_http_push_url_userinfo(push_url: &str) -> Result<(), ForgeError> {
    let Some((scheme, remainder)) = push_url.split_once("://") else {
        return Ok(());
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Ok(());
    }
    let authority = remainder.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(validation(
            "push_destination_credentials_unsupported",
            "HTTP(S) push URLs with userinfo are unsupported for direct-main delivery",
            Some("use a credential helper and a URL without embedded userinfo".into()),
        ));
    }
    Ok(())
}

fn bind_remote_repository(remote_url: &str, repository: &str) -> Result<(), ForgeError> {
    let remote_slug = parse_slug(remote_url).ok_or_else(|| {
        validation(
            "repository_mismatch",
            "the selected Git remote URL does not identify an owner/repository slug",
            None,
        )
    })?;
    if !remote_slug.eq_ignore_ascii_case(repository) {
        return Err(validation(
            "repository_mismatch",
            format!(
                "provider repository '{repository}' does not match selected Git remote '{remote_slug}'"
            ),
            None,
        ));
    }
    Ok(())
}

fn metadata_repo_locator(provider: Provider, host: &str, repository: &str) -> String {
    let host = canonical_forge_host(host);
    match provider {
        Provider::GitHub => format!("{host}/{repository}"),
        Provider::GitLab => format!("https://{host}/{repository}"),
        Provider::Local => repository.to_string(),
    }
}

fn bind_remote_metadata(
    push_url: &str,
    metadata_url: &str,
    repository: &str,
) -> Result<(), ForgeError> {
    let push_host = parse_host(push_url)
        .map(|host| canonical_forge_host(&host))
        .ok_or_else(|| {
            validation(
                "repository_mismatch",
                "the selected Git push URL does not expose a forge host",
                None,
            )
        })?;
    let metadata_host = parse_host(metadata_url)
        .map(|host| canonical_forge_host(&host))
        .ok_or_else(|| {
            validation(
                "repository_mismatch",
                "provider repository metadata does not expose a forge host",
                None,
            )
        })?;
    let metadata_slug = parse_slug(metadata_url).ok_or_else(|| {
        validation(
            "repository_mismatch",
            "provider repository metadata does not identify an owner/repository slug",
            None,
        )
    })?;
    if push_host != metadata_host || !metadata_slug.eq_ignore_ascii_case(repository) {
        return Err(validation(
            "repository_mismatch",
            format!(
                "provider repository metadata '{metadata_host}/{metadata_slug}' does not match push destination '{push_host}/{repository}'"
            ),
            None,
        ));
    }
    Ok(())
}

fn canonical_forge_host(host: &str) -> String {
    match host.trim().to_ascii_lowercase().as_str() {
        "ssh.github.com" => "github.com".to_string(),
        "altssh.gitlab.com" => "gitlab.com".to_string(),
        other => other.to_string(),
    }
}

fn bind_remote_provider(remote_url: &str, provider: Provider) -> Result<(), ForgeError> {
    if provider == Provider::Local {
        return Err(ForgeError::provider_unsupported(
            schema_error(),
            "repo push-default does not support the local provider",
            None,
        ));
    }
    let remote_host = parse_host(remote_url).ok_or_else(|| {
        validation(
            "provider_mismatch",
            "the selected Git remote URL does not expose a forge host",
            None,
        )
    })?;
    let remote_provider = classify_host(&remote_host).ok_or_else(|| {
        ForgeError::provider_unsupported(
            schema_error(),
            format!("unsupported forge host for repo push-default: {remote_host}"),
            None,
        )
    })?;
    if remote_provider != provider {
        return Err(validation(
            "provider_mismatch",
            format!(
                "selected provider '{}' does not match Git remote host '{remote_host}'",
                provider.as_str()
            ),
            None,
        ));
    }
    Ok(())
}

fn validate_object_id(value: &str) -> Result<String, ForgeError> {
    let value = value.trim();
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(validation(
            "object_id_invalid",
            "expected a full 40- or 64-character hexadecimal commit SHA",
            None,
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn ls_remote_head<G: GitRunner>(
    git: &G,
    workdir: &Path,
    destination: &str,
    remote_label: &str,
    default_ref: &str,
) -> Result<String, ForgeError> {
    let output = git.run(
        workdir,
        &os_args(&["ls-remote", "--exit-code", "--", destination, default_ref]),
    )?;
    if !output.success && output.exit_code == 2 && output.stdout.trim().is_empty() {
        return Err(validation(
            "remote_default_branch_missing",
            format!("remote '{remote_label}' does not expose {default_ref}"),
            None,
        ));
    }
    if !output.success {
        return Err(ForgeError::unavailable(
            schema_error(),
            "remote_default_lookup_failed",
            format!("failed to read {default_ref} from remote '{remote_label}'"),
            (!output.stderr.is_empty()).then_some(output.stderr),
        ));
    }
    let output = output.stdout;
    let mut rows = output.lines().filter(|line| !line.trim().is_empty());
    let row = rows.next().ok_or_else(|| {
        validation(
            "remote_default_branch_missing",
            format!("remote '{remote_label}' does not expose {default_ref}"),
            None,
        )
    })?;
    if rows.next().is_some() {
        return Err(ForgeError::software(
            schema_error(),
            "git ls-remote returned multiple rows for one exact default-branch ref",
            None,
        ));
    }
    let mut fields = row.split_whitespace();
    let sha = fields.next().unwrap_or_default();
    let found_ref = fields.next().unwrap_or_default();
    if found_ref != default_ref || fields.next().is_some() {
        return Err(ForgeError::software(
            schema_error(),
            "git ls-remote returned an unexpected default-branch row",
            None,
        ));
    }
    validate_object_id(sha)
}

fn git_capture<G: GitRunner>(
    git: &G,
    workdir: &Path,
    args: &[&str],
    operation: &str,
) -> Result<String, ForgeError> {
    let output = git.run(workdir, &os_args(args))?;
    if !output.success {
        return Err(ForgeError::software(
            schema_error(),
            format!("git {operation} failed"),
            (!output.stderr.is_empty()).then_some(output.stderr),
        ));
    }
    Ok(output.stdout)
}

fn os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsStr::new).map(OsString::from).collect()
}

fn render_text(payload: &RepoPushDefaultPayload) {
    if payload.pushed {
        println!(
            "pushed {sha} to {remote}/{branch}\nbase: {base}\nreason: {reason}\nremote read-back: {observed}",
            sha = payload.head_sha,
            remote = payload.remote,
            branch = payload.default_branch,
            base = payload.expected_base,
            reason = payload.reason,
            observed = payload.observed_remote_sha,
        );
    } else {
        println!(
            "would push {sha} to {remote}/{branch}\nbase: {base}\nreason: {reason}",
            sha = payload.head_sha,
            remote = payload.remote,
            branch = payload.default_branch,
            base = payload.expected_base,
            reason = payload.reason,
        );
    }
}

fn validation(
    kind: &'static str,
    message: impl Into<String>,
    detail: Option<String>,
) -> ForgeError {
    ForgeError::validation(schema_error(), kind, message, detail)
}

fn schema_success() -> String {
    schema_version_for(BINARY, SCHEMA, SCHEMA_VERSION)
}

fn schema_error() -> String {
    schema_version_for(BINARY, "error", 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_provider_binding_rejects_forced_cross_provider_delivery() {
        let error = bind_remote_provider("git@github.com:sympoies/demo.git", Provider::GitLab)
            .expect_err("cross-provider remote must fail closed");
        assert_eq!(error.kind(), "provider_mismatch");
    }

    #[test]
    fn remote_provider_binding_rejects_local_backend_delivery() {
        let error = bind_remote_provider("git@github.com:sympoies/demo.git", Provider::Local)
            .expect_err("local provider cannot authorize a real Git push");
        assert_eq!(error.kind(), "provider_unsupported");
    }
}
