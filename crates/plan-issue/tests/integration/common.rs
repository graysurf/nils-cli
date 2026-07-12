use std::path::Path;
use std::sync::LazyLock;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};

/// Hermetic per-process `$PLAN_ISSUE_HOME` for every baseline invocation.
///
/// Without this, parallel tests share the host-default state dir
/// (`$XDG_STATE_HOME/plan-issue`) and race on lifecycle locks whose key is
/// the fixture repo/issue/profile triple, so a victim test exits 1 with an
/// empty stderr (issue: plan-tracking-testbed#61). nextest runs one process
/// per test, so a process-wide dir isolates each test while multi-invocation
/// tests keep state continuity within their own process.
static HERMETIC_STATE_DIR: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
    tempfile::Builder::new()
        .prefix("plan-issue-state-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("create hermetic plan-issue state dir")
});

/// Build a deterministic baseline for plan-issue integration tests.
/// Tests can compose env/path overrides via `CmdOptions` instead of ad-hoc
/// shell-style setup in each test body; a later `with_env` push or an
/// explicit `--state-dir` flag still overrides the hermetic baseline.
pub fn plan_issue_cmd_options() -> CmdOptions {
    CmdOptions::new()
        .with_cwd(Path::new(env!("CARGO_MANIFEST_DIR")))
        .with_env(
            "PLAN_ISSUE_HOME",
            HERMETIC_STATE_DIR
                .path()
                .to_str()
                .expect("hermetic state dir path is utf-8"),
        )
}

#[allow(dead_code)]
pub fn run_plan_issue(args: &[&str]) -> CmdOutput {
    run_resolved("plan-issue", args, &plan_issue_cmd_options())
}

#[allow(dead_code)]
pub fn run_plan_issue_with_options(args: &[&str], options: CmdOptions) -> CmdOutput {
    run_resolved("plan-issue", args, &options)
}

#[allow(dead_code)]
pub fn run_plan_issue_local(args: &[&str]) -> CmdOutput {
    run_resolved("plan-issue-local", args, &plan_issue_cmd_options())
}

#[allow(dead_code)]
pub fn run_plan_issue_local_with_env(args: &[&str], env: &[(&str, &str)]) -> CmdOutput {
    run_resolved(
        "plan-issue-local",
        args,
        &plan_issue_cmd_options().with_envs(env),
    )
}

/// A `forge-cli` stub that emits v1 JSON envelopes, replacing the previous
/// direct-`gh` stubs after plan-issue's GitHub provider ops were consolidated
/// onto `forge-cli` (`docs/plans/2026-06-19-plan-issue-forge-cli-consolidation`).
///
/// plan-issue's [`ForgeCliAdapter`] now spawns `forge-cli` for every provider
/// op (overridable via `FORGE_CLI_BIN`), passing
/// `--format json --provider <p> --repo <slug> <subcommand> ...`. This stub
/// recognises the issue/PR subcommands plan-issue uses and returns the
/// `{ok:true, schema_version, data}` envelope shape the adapter parses.
///
/// Env knobs (mirroring the retired gh stub, but `FORGE_CLI_STUB_`-prefixed so
/// a `with_env_remove_prefix("FORGE_CLI_STUB_")` baseline keeps them
/// deterministic):
/// - `FORGE_CLI_STUB_LOG`: append each invocation's `provider`-stripped argv
///   to this file. Lines look like `issue view 217 --repo o/r ...` so existing
///   `issue view <n>` / `pr view <n>` substring assertions keep matching.
/// - `FORGE_CLI_STUB_VIEW_BODY_JSON`: the JSON-encoded string value for the
///   `body` field of `issue view` (e.g. `serde_json::json!(body).to_string()`,
///   already quoted + escaped). Defaults to `""`.
/// - `FORGE_CLI_STUB_VIEW_COMMENTS_JSON`: JSON array for the `comments` field
///   of `issue view --with-comments`. Defaults to `[]`.
/// - `FORGE_CLI_STUB_CREATE_URL`: URL returned by `issue create`.
/// - `FORGE_CLI_STUB_COMMENT_URL`: URL returned by `issue comment`.
/// - `FORGE_CLI_STUB_EDIT_LABELS_JSON`: provider-observed JSON label array
///   returned by `issue edit`. Defaults to `[]`.
/// - `FORGE_CLI_STUB_REPO_LABELS_JSON`: repository label catalog returned by
///   `label list`. Defaults to the three lifecycle labels used by close tests.
/// - `FORGE_CLI_STUB_STRICT_REPO_LABELS=1`: reject `issue edit --add-label`
///   when the requested label is absent from the repository catalog.
/// - `FORGE_CLI_STUB_LABELS_FILE`: optional newline-delimited provider label
///   state shared across stub invocations.
/// - `FORGE_CLI_STUB_ISSUE_STATE_FILE`: optional provider issue-state file
///   shared across stub invocations.
/// - `FORGE_CLI_STUB_DROP_LABEL_MUTATIONS=1`: report successful label edits
///   without changing `FORGE_CLI_STUB_LABELS_FILE`.
/// - `FORGE_CLI_STUB_CAPTURE_BODY_FILE`: copy the `issue edit --body-file`
///   payload here.
/// - `FORGE_CLI_STUB_CAPTURE_COMMENT_FILE`: copy the `issue comment
///   --body-file` payload here.
/// - `FORGE_CLI_STUB_UNMERGED_PRS`: comma-list of PR numbers reported `open`.
/// - `FORGE_CLI_STUB_CHECKS_STATE` / `FORGE_CLI_STUB_REQUIRED_COUNT`: override
///   the `pr checks` rollup (default success / 0).
/// - `FORGE_CLI_STUB_PR_COMMENTS_JSON`: JSON array for the `comments` field of
///   `pr comments`. Defaults to `[]`.
/// - `FORGE_CLI_STUB_NO_MD_GUARD=1`: disable the escaped-control markdown guard
///   on write ops (for tests exercising unrelated paths).
/// - `FORGE_CLI_STUB_NO_PATH_GUARD=1`: disable the machine-local-home-path
///   guard on `issue comment` / `issue edit` write ops.
#[allow(dead_code)]
pub fn forge_cli_stub_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail

# Strip the leading `--format json --provider <p> --repo <slug>` prefix that
# plan-issue's ForgeCliAdapter always emits, and log the remainder so the
# checked-in `issue view <n>` / `pr view <n>` substring assertions still match.
provider="github"
repo=""
body_file=""
add_labels=()
remove_labels=()
positionals=()
logged=()
prev=""
skip_repo_value=0
for arg in "$@"; do
  case "$prev" in
    --provider) provider="$arg" ;;
    --repo) repo="$arg" ;;
    --body-file) body_file="$arg" ;;
    --add-label) add_labels+=("$arg") ;;
    --remove-label) remove_labels+=("$arg") ;;
  esac
  # Build the logged argv: drop `--format json` and `--provider <p>` entirely,
  # keep everything else (including `--repo <slug>`).
  if [[ "$prev" == "--format" || "$prev" == "--provider" ]]; then
    prev="$arg"; continue
  fi
  if [[ "$arg" == "--format" || "$arg" == "--provider" ]]; then
    prev="$arg"; continue
  fi
  logged+=("$arg")
  case "$arg" in
    --*) ;;
    *)
      case "$prev" in
        --repo|--title|--body|--body-file|--label|--assignee|--add-label|--remove-label|--reason|--state|--limit|--author|--assignee) ;;
        *) positionals+=("$arg") ;;
      esac
      ;;
  esac
  prev="$arg"
done

if [[ -n "${FORGE_CLI_STUB_LOG:-}" ]]; then
  printf '%s\n' "${logged[*]}" >> "$FORGE_CLI_STUB_LOG"
fi

group="${positionals[0]:-}"
verb="${positionals[1]:-}"
id="${positionals[2]:-0}"

emit() { printf '%s\n' "$1"; }

issue_state() {
  if [[ -n "${FORGE_CLI_STUB_ISSUE_STATE_FILE:-}" && -f "$FORGE_CLI_STUB_ISSUE_STATE_FILE" ]]; then
    cat "$FORGE_CLI_STUB_ISSUE_STATE_FILE"
  else
    printf 'open'
  fi
}

provider_labels_json() {
  if [[ -z "${FORGE_CLI_STUB_LABELS_FILE:-}" ]]; then
    printf '%s' "${FORGE_CLI_STUB_EDIT_LABELS_JSON:-[]}"
    return
  fi
  local json='[' separator='' label escaped
  while IFS= read -r label || [[ -n "$label" ]]; do
    [[ -z "$label" ]] && continue
    escaped="${label//\\/\\\\}"
    escaped="${escaped//\"/\\\"}"
    json+="${separator}\"${escaped}\""
    separator=','
  done < "$FORGE_CLI_STUB_LABELS_FILE"
  printf '%s]' "$json"
}

repo_labels_json() {
  if [[ -n "${FORGE_CLI_STUB_REPO_LABELS_JSON:-}" ]]; then
    printf '%s' "$FORGE_CLI_STUB_REPO_LABELS_JSON"
  else
    printf '%s' '[{"name":"state::needs-triage","color":"000000","description":""},{"name":"state::ready","color":"000000","description":""},{"name":"state::closed","color":"000000","description":""}]'
  fi
}

emit_escaped_control_error() {
  # Mirror forge-cli's `markdown_escaped_control` validation error envelope so
  # the post-consolidation GitHub write path (which now relies on forge-cli's
  # guard, not a plan-issue-side guard) rejects literal escaped-control
  # payloads. Reports the offending sequence and the forge-cli fix hint.
  emit "{\"ok\":false,\"schema_version\":\"cli.forge-cli.error.v1\",\"error\":{\"code\":\"markdown_escaped_control\",\"message\":\"markdown payload contains literal escaped-control artifacts: \\\\n (1). Replace escaped controls (\\\\n / \\\\r / \\\\t) with real characters or wrap them in a code span.\"}}"
  exit 1
}

# forge-cli rejects literal `\n` / `\r` / `\t` in write-op bodies (prose,
# outside code spans). The stub approximates that by scanning the captured
# body-file for a literal backslash-n sequence. `FORGE_CLI_STUB_NO_MD_GUARD=1`
# disables the check for tests that exercise unrelated paths.
maybe_reject_escaped_control() {
  if [[ "${FORGE_CLI_STUB_NO_MD_GUARD:-}" == "1" ]]; then return; fi
  if [[ -n "$body_file" && -f "$body_file" ]]; then
    if grep -q '\\n' "$body_file" || grep -q '\\r' "$body_file" || grep -q '\\t' "$body_file"; then
      emit_escaped_control_error
    fi
  fi
}

emit_local_path_error() {
  # Mirror forge-cli's `no_local_path` (`local_path_present`) validation error.
  # The adapter surfaces only `code: message`, and forge-cli's `message` is the
  # generic count line — the per-line `$HOME/...` suggestion lives in `detail`,
  # which the adapter does not surface — so the message here matches
  # `LocalPathError::message()`.
  emit "{\"ok\":false,\"schema_version\":\"cli.forge-cli.error.v1\",\"error\":{\"code\":\"local_path_present\",\"message\":\"$1 contains 1 machine-local home path(s); use \$HOME-relative paths\"}}"
  exit 1
}

# forge-cli rejects machine-local home paths (`/Users/<owner>/...`,
# `/home/<owner>/...`, excluding the container/runner allowlist) in write-op
# bodies. The stub mirrors that for the `issue comment` / `issue edit` write
# path so the post-consolidation privacy gate (now enforced by forge-cli, not a
# plan-issue-side guard) still rejects unsafe payloads. The `$1` arg names the
# forge-cli source field (`comment` / `body`). `FORGE_CLI_STUB_NO_PATH_GUARD=1`
# disables the check.
maybe_reject_local_path() {
  if [[ "${FORGE_CLI_STUB_NO_PATH_GUARD:-}" == "1" ]]; then return; fi
  if [[ -n "$body_file" && -f "$body_file" ]]; then
    if grep -Eq '/Users/[A-Za-z0-9._-]+|/home/[A-Za-z0-9._-]+' "$body_file" \
       && ! grep -Eq '^/(home/(agent|linuxbrew|runner))' "$body_file"; then
      emit_local_path_error "$1"
    fi
  fi
}

case "$group $verb" in
  "issue view")
    body_json="${FORGE_CLI_STUB_VIEW_BODY_JSON:-\"\"}"
    comments_json="${FORGE_CLI_STUB_VIEW_COMMENTS_JSON:-[]}"
    state="$(issue_state)"
    labels_json="$(provider_labels_json)"
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.issue.view.v1\",\"data\":{\"provider\":\"$provider\",\"number\":$id,\"url\":\"https://github.com/$repo/issues/$id\",\"state\":\"$state\",\"title\":\"t\",\"body\":$body_json,\"labels\":$labels_json,\"assignees\":[],\"comments\":$comments_json}}"
    ;;
  "issue create")
    url="${FORGE_CLI_STUB_CREATE_URL:-https://github.com/sympoies/nils-cli/issues/999}"
    num="${url##*/}"
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.issue.create.v1\",\"data\":{\"provider\":\"$provider\",\"number\":$num,\"url\":\"$url\",\"title\":\"t\",\"state\":\"open\",\"labels\":[],\"assignees\":[]}}"
    ;;
  "issue edit")
    maybe_reject_local_path "body"
    maybe_reject_escaped_control
    if [[ -n "${FORGE_CLI_STUB_CAPTURE_BODY_FILE:-}" && -n "$body_file" ]]; then
      cp "$body_file" "$FORGE_CLI_STUB_CAPTURE_BODY_FILE"
    fi
    if [[ "${FORGE_CLI_STUB_STRICT_REPO_LABELS:-}" == "1" ]]; then
      catalog="$(repo_labels_json)"
      for label in "${add_labels[@]-}"; do
        [[ -z "$label" ]] && continue
        if ! printf '%s' "$catalog" | grep -Fq "\"$label\""; then
          emit "{\"ok\":false,\"schema_version\":\"cli.forge-cli.error.v1\",\"error\":{\"code\":\"label-not-found\",\"message\":\"label \\\"$label\\\" not found\"}}"
          exit 1
        fi
      done
    fi
    if [[ -n "${FORGE_CLI_STUB_LABELS_FILE:-}" && "${FORGE_CLI_STUB_DROP_LABEL_MUTATIONS:-}" != "1" ]]; then
      touch "$FORGE_CLI_STUB_LABELS_FILE"
      # Bash 3.2 treats an empty array as unset under `set -u`; the `-`
      # fallback keeps body-only edits portable while preserving array items.
      for label in "${remove_labels[@]-}"; do
        [[ -z "$label" ]] && continue
        grep -Fvx -- "$label" "$FORGE_CLI_STUB_LABELS_FILE" > "${FORGE_CLI_STUB_LABELS_FILE}.tmp" || true
        mv "${FORGE_CLI_STUB_LABELS_FILE}.tmp" "$FORGE_CLI_STUB_LABELS_FILE"
      done
      for label in "${add_labels[@]-}"; do
        [[ -z "$label" ]] && continue
        grep -Fxq -- "$label" "$FORGE_CLI_STUB_LABELS_FILE" || printf '%s\n' "$label" >> "$FORGE_CLI_STUB_LABELS_FILE"
      done
    fi
    state="$(issue_state)"
    labels_json="$(provider_labels_json)"
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.issue.edit.v1\",\"data\":{\"provider\":\"$provider\",\"number\":$id,\"url\":\"https://github.com/$repo/issues/$id\",\"state\":\"$state\",\"title\":\"t\",\"labels\":$labels_json,\"assignees\":[]}}"
    ;;
  "issue comment")
    maybe_reject_local_path "comment"
    maybe_reject_escaped_control
    if [[ -n "${FORGE_CLI_STUB_CAPTURE_COMMENT_FILE:-}" && -n "$body_file" ]]; then
      cp "$body_file" "$FORGE_CLI_STUB_CAPTURE_COMMENT_FILE"
    fi
    url="${FORGE_CLI_STUB_COMMENT_URL:-https://github.com/$repo/issues/${id}#issuecomment-1}"
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.issue.comment.v1\",\"data\":{\"provider\":\"$provider\",\"number\":$id,\"url\":\"$url\"}}"
    ;;
  "issue close")
    if [[ -n "${FORGE_CLI_STUB_ISSUE_STATE_FILE:-}" ]]; then
      printf 'closed\n' > "$FORGE_CLI_STUB_ISSUE_STATE_FILE"
    fi
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.issue.close.v1\",\"data\":{\"provider\":\"$provider\",\"number\":$id,\"url\":\"https://github.com/$repo/issues/$id\",\"state\":\"closed\"}}"
    ;;
  "issue list")
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.issue.list.v1\",\"data\":{\"provider\":\"$provider\",\"items\":[]}}"
    ;;
  "label list")
    labels_json="$(repo_labels_json)"
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.label.list.v1\",\"data\":{\"provider\":\"$provider\",\"labels\":$labels_json}}"
    ;;
  "pr view")
    if [[ ",${FORGE_CLI_STUB_UNMERGED_PRS:-}," == *",${id},"* ]]; then
      emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.pr.view.v1\",\"data\":{\"provider\":\"$provider\",\"number\":$id,\"url\":\"https://github.com/$repo/pull/$id\",\"state\":\"open\",\"draft\":false,\"title\":\"t\",\"head\":\"x\",\"base\":\"main\",\"mergeable\":\"yes\",\"merged_at\":null,\"merge_commit_sha\":null,\"labels\":[]}}"
    else
      emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.pr.view.v1\",\"data\":{\"provider\":\"$provider\",\"number\":$id,\"url\":\"https://github.com/$repo/pull/$id\",\"state\":\"merged\",\"draft\":false,\"title\":\"t\",\"head\":\"x\",\"base\":\"main\",\"mergeable\":\"yes\",\"merged_at\":\"2026-02-25T00:00:00Z\",\"merge_commit_sha\":\"deadbeef\",\"labels\":[]}}"
    fi
    ;;
  "pr checks")
    state="${FORGE_CLI_STUB_CHECKS_STATE:-success}"
    rcount="${FORGE_CLI_STUB_REQUIRED_COUNT:-0}"
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.pr.checks.v1\",\"data\":{\"provider\":\"$provider\",\"state\":\"$state\",\"required_count\":$rcount,\"success_count\":$rcount,\"failed\":[],\"pending\":[],\"checks\":[]}}"
    ;;
  "pr comments")
    comments_json="${FORGE_CLI_STUB_PR_COMMENTS_JSON:-[]}"
    emit "{\"ok\":true,\"schema_version\":\"cli.forge-cli.pr.comments.v1\",\"data\":{\"provider\":\"$provider\",\"number\":$id,\"url\":\"https://github.com/$repo/pull/$id\",\"comments\":$comments_json}}"
    ;;
  *)
    printf 'unsupported forge-cli call: %s\n' "$*" >&2
    exit 1
    ;;
esac
"#
}

/// Reserved hook for tests that historically pre-seeded
/// `$PLAN_ISSUE_HOME/prompts/` before plan-issue copied init snapshots into
/// each runtime workspace. The init-snapshot copy was removed in the
/// 0.8 cut, so callers no longer need fixture content — the helper now
/// only verifies the workspace path is a directory.
#[allow(dead_code)]
pub fn ensure_state_dir(state_dir: &Path) {
    std::fs::create_dir_all(state_dir).expect("create plan-issue state-dir");
}
