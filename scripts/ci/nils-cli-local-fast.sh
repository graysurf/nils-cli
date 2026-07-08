#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/nils-cli-local-fast.sh [--base <ref>] [--plan-only] [--changed-file <path>]...

Description:
  Fast local validation for day-to-day development.
  - Detects changed files against a merge-base with <ref> plus staged,
    unstaged, and untracked files.
  - Runs docs-only checks for documentation-only changes.
  - Runs the docs-hygiene audit for every non-empty change set (it scans
    runtime surfaces such as crates/**/*.rs, not just doc paths), mirroring the
    unconditional CI run so a code-only diff that trips a hygiene guardrail
    fails here too.
  - Runs third-party artifact audit when Cargo manifests, Cargo.lock, the
    generator scripts, or the generated third-party artifact files change.
  - Runs package-scoped fmt/clippy/tests for non-shared crate changes.
  - Escalates to a workspace Rust gate for shared crates or workspace-level
    files where package-scoped checks can miss reverse-dependency breakage.

Options:
  --base <ref>            Diff base for committed changes.
                          Default: NILS_CLI_LOCAL_FAST_BASE or origin/main.
  --plan-only             Print the detected validation plan and do not run it.
  --changed-file <path>   Override changed-file detection. Repeatable.
                          Intended for regression tests and explicit scopes.
  -h, --help              Show this help.

Environment:
  NILS_CLI_LOCAL_FAST_BASE
    Default base ref when --base is not supplied.
  NILS_CLI_TEST_RUNNER
    auto, nextest, cargo, or cargo-test. Default: auto.
USAGE
}

base="${NILS_CLI_LOCAL_FAST_BASE:-origin/main}"
plan_only=0
declare -a forced_changed_files=()

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --base)
      if [[ $# -lt 2 ]]; then
        echo "error: --base requires a value" >&2
        exit 2
      fi
      base="${2:-}"
      shift 2
      ;;
    --base=*)
      base="${1#--base=}"
      shift
      ;;
    --plan-only)
      plan_only=1
      shift
      ;;
    --changed-file)
      if [[ $# -lt 2 ]]; then
        echo "error: --changed-file requires a value" >&2
        exit 2
      fi
      forced_changed_files+=("${2:-}")
      shift 2
      ;;
    --changed-file=*)
      forced_changed_files+=("${1#--changed-file=}")
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: ${1:-}" >&2
      usage >&2
      exit 2
      ;;
  esac
done

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: missing required tool on PATH: $cmd" >&2
    exit 2
  fi
}

require_cmd git
require_cmd python3

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

run() {
  local -a cmd=( "$@" )
  echo "+ ${cmd[*]}"
  if "${cmd[@]}"; then
    return 0
  else
    local code=$?
    echo "error: local-fast check failed (exit $code): ${cmd[*]}" >&2
    exit 1
  fi
}

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/nils-cli-local-fast.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
changed_file_list="$tmp_dir/changed-files.txt"
plan_file="$tmp_dir/plan.tsv"

collect_changed_files() {
  if [[ "${#forced_changed_files[@]}" -gt 0 ]]; then
    printf '%s\n' "${forced_changed_files[@]}" | sed '/^$/d' | sort -u
    return 0
  fi

  if ! git rev-parse --verify "${base}^{commit}" >/dev/null 2>&1; then
    echo "error: base ref does not resolve to a commit: $base" >&2
    exit 2
  fi

  local merge_base
  merge_base="$(git merge-base HEAD "$base")"
  {
    git diff --name-only --diff-filter=ACDMRT "$merge_base"...HEAD
    git diff --name-only --diff-filter=ACDMRT
    git diff --name-only --cached --diff-filter=ACDMRT
    git ls-files --others --exclude-standard
  } | sed '/^$/d' | sort -u
}

collect_changed_files >"$changed_file_list"

python3 - "$repo_root" "$changed_file_list" >"$plan_file" <<'PY'
import json
import pathlib
import shutil
import subprocess
import sys

repo = pathlib.Path(sys.argv[1])
changed_path = pathlib.Path(sys.argv[2])
changed = [
    line.strip()
    for line in changed_path.read_text(encoding="utf-8").splitlines()
    if line.strip()
]

# is_doc_path / affects_third_party_artifacts live in scripts/ci/lib so the CI
# docs-only gate (scripts/ci/detect-docs-only.sh) shares one definition.
sys.path.insert(0, str(repo / "scripts" / "ci" / "lib"))
from doc_classify import affects_third_party_artifacts, is_doc_path


def emit(key, value):
    print(f"{key}\t{value}")


if not changed:
    emit("mode", "none")
    emit("docs_checks", "0")
    emit("docs_hygiene", "0")
    emit("changed_count", "0")
    sys.exit(0)

third_party_artifacts = any(affects_third_party_artifacts(path) for path in changed)

if all(is_doc_path(path) for path in changed) and not third_party_artifacts:
    emit("mode", "docs-only")
    emit("docs_checks", "1")
    # docs-hygiene runs as part of the docs-only battery on this lane.
    emit("docs_hygiene", "1")
    emit("third_party_artifacts", "0")
    emit("changed_count", str(len(changed)))
    for path in changed:
        emit("changed", path)
    sys.exit(0)

if shutil.which("cargo") is None:
    print("error: cargo is required for non-document local-fast planning", file=sys.stderr)
    sys.exit(2)

metadata_raw = subprocess.check_output(
    ["cargo", "metadata", "--no-deps", "--format-version", "1"],
    cwd=repo,
    text=True,
)
metadata = json.loads(metadata_raw)

crate_roots = []
package_has_doctests = {}
for package in metadata["packages"]:
    manifest = pathlib.Path(package["manifest_path"])
    root = manifest.parent.relative_to(repo).as_posix()
    has_doctests = any(target.get("doctest") for target in package.get("targets", []))
    crate_roots.append((root, package["name"]))
    package_has_doctests[package["name"]] = has_doctests
crate_roots.sort(key=lambda item: len(item[0]), reverse=True)

shared_packages = {"nils-common", "nils-term", "nils-test-support", "nils-scrub"}


def package_for_path(path):
    for root, name in crate_roots:
        if path == root or path.startswith(root + "/"):
            return root, name
    return None


packages = set()
workspace_reasons = []
shell_files = []
docs_checks = False

for path in changed:
    if path.endswith(".sh") and (repo / path).is_file():
        shell_files.append(path)

    if path in {"THIRD_PARTY_LICENSES.md", "THIRD_PARTY_NOTICES.md"}:
        workspace_reasons.append(f"third-party artifact output changed: {path}")

    if is_doc_path(path):
        docs_checks = True
        continue

    package = package_for_path(path)
    if package is not None:
        _root, package_name = package
        if package_name in shared_packages:
            workspace_reasons.append(f"shared package changed: {package_name}")
        else:
            packages.add(package_name)
        continue

    if path in {"Cargo.toml", "Cargo.lock"}:
        workspace_reasons.append(f"workspace manifest changed: {path}")
    elif path.startswith((".agents/", ".github/", ".cargo/", ".config/", "scripts/", "tests/", "completions/")):
        workspace_reasons.append(f"workspace-level path changed: {path}")
    else:
        workspace_reasons.append(f"unclassified workspace path changed: {path}")

if not changed:
    mode = "none"
elif workspace_reasons:
    mode = "workspace"
elif packages:
    mode = "packages"
elif docs_checks:
    mode = "docs-only"
else:
    mode = "workspace"
    workspace_reasons.append("fallback workspace mode")

emit("mode", mode)
emit("docs_checks", "1" if docs_checks else "0")
# docs-hygiene-audit scans runtime surfaces (crates/**/*.rs and specific source
# files), not just doc paths, and CI runs it unconditionally for every
# non-docs-only change. Signal it for every non-empty change set so a Rust-only
# diff that trips a docs-hygiene guardrail fails local-fast, not just full CI.
emit("docs_hygiene", "0" if mode == "none" else "1")
emit("third_party_artifacts", "1" if third_party_artifacts else "0")
emit("changed_count", str(len(changed)))
for path in changed:
    emit("changed", path)
for package_name in sorted(packages):
    emit("package", package_name)
for package_name in sorted(packages):
    if package_has_doctests.get(package_name, False):
        emit("package_doctest", package_name)
for reason in sorted(set(workspace_reasons)):
    emit("reason", reason)
for shell_file in sorted(set(shell_files)):
    emit("shell", shell_file)
PY

mode=""
docs_checks=0
docs_hygiene=0
third_party_artifacts=0
changed_count=0
declare -a packages=()
declare -a package_doctests=()
declare -a reasons=()
declare -a changed_files=()
declare -a shell_files=()

while IFS=$'\t' read -r key value; do
  case "$key" in
    mode) mode="$value" ;;
    docs_checks) docs_checks="$value" ;;
    docs_hygiene) docs_hygiene="$value" ;;
    third_party_artifacts) third_party_artifacts="$value" ;;
    changed_count) changed_count="$value" ;;
    package) packages+=("$value") ;;
    package_doctest) package_doctests+=("$value") ;;
    reason) reasons+=("$value") ;;
    changed) changed_files+=("$value") ;;
    shell) shell_files+=("$value") ;;
  esac
done <"$plan_file"

if [[ -z "$mode" ]]; then
  echo "error: local-fast planner did not emit a mode" >&2
  exit 1
fi

print_plan() {
  echo "LOCAL_FAST_MODE=$mode"
  echo "LOCAL_FAST_BASE=$base"
  echo "LOCAL_FAST_DOCS_CHECKS=$docs_checks"
  echo "LOCAL_FAST_DOCS_HYGIENE=$docs_hygiene"
  echo "LOCAL_FAST_THIRD_PARTY_ARTIFACTS=$third_party_artifacts"
  echo "LOCAL_FAST_CHANGED_COUNT=$changed_count"
  for path in "${changed_files[@]}"; do
    echo "LOCAL_FAST_CHANGED=$path"
  done
  for package in "${packages[@]}"; do
    echo "LOCAL_FAST_PACKAGE=$package"
  done
  for package in "${package_doctests[@]}"; do
    echo "LOCAL_FAST_PACKAGE_DOCTEST=$package"
  done
  for reason in "${reasons[@]}"; do
    echo "LOCAL_FAST_REASON=$reason"
  done
  for shell_file in "${shell_files[@]}"; do
    echo "LOCAL_FAST_SHELL=$shell_file"
  done
}

print_plan

if [[ "$plan_only" -eq 1 ]]; then
  exit 0
fi

bash_bin="${BASH:-}"
if [[ -z "$bash_bin" || ! -x "$bash_bin" ]]; then
  bash_bin="$(command -v bash || true)"
fi
if [[ -z "$bash_bin" || ! -x "$bash_bin" ]]; then
  echo "error: bash not found on PATH" >&2
  exit 2
fi

verify_script="./.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh"
if [[ ! -f "$verify_script" ]]; then
  echo "error: missing required checks script: $verify_script" >&2
  exit 2
fi

run_docs_checks() {
  run "$bash_bin" "$verify_script" --docs-only
}

select_test_runner() {
  local requested="${NILS_CLI_TEST_RUNNER:-auto}"
  case "$requested" in
    ""|auto)
      if command -v cargo-nextest >/dev/null 2>&1; then
        echo "nextest"
      else
        echo "cargo"
      fi
      ;;
    nextest)
      if ! command -v cargo-nextest >/dev/null 2>&1; then
        echo "error: NILS_CLI_TEST_RUNNER=nextest requires cargo-nextest on PATH" >&2
        exit 2
      fi
      echo "nextest"
      ;;
    cargo|cargo-test)
      echo "cargo"
      ;;
    *)
      echo "error: unsupported NILS_CLI_TEST_RUNNER value: $requested (expected auto, cargo, or nextest)" >&2
      exit 2
      ;;
  esac
}

package_has_doctest() {
  local package="$1"
  local candidate
  for candidate in "${package_doctests[@]}"; do
    if [[ "$candidate" == "$package" ]]; then
      return 0
    fi
  done
  return 1
}

case "$mode" in
  none)
    echo "ok: local-fast found no changed files"
    exit 0
    ;;
  docs-only)
    run_docs_checks
    echo "ok: local-fast docs-only checks passed"
    exit 0
    ;;
  packages|workspace)
    ;;
  *)
    echo "error: unsupported local-fast mode: $mode" >&2
    exit 1
    ;;
esac

if [[ "$docs_checks" -eq 1 ]]; then
  run_docs_checks
elif [[ "$docs_hygiene" -eq 1 ]]; then
  # No doc path changed, so the full docs-only battery is not selected, but
  # docs-hygiene-audit scans runtime surfaces (crates/**/*.rs and specific
  # source files) that a code-only diff can trip. CI runs docs-hygiene
  # unconditionally for non-docs-only changes; run it standalone here to match,
  # keeping this code-only path free of the battery's extra tool requirements.
  #
  # docs-hygiene-audit.sh guards its keyword scans with `rg ... || true`, so a
  # missing rg is swallowed and the audit prints PASS. Require rg first so this
  # code-only branch cannot report a false green while silently skipping the
  # Rust-source guardrail it exists to enforce.
  require_cmd rg
  run bash scripts/ci/docs-hygiene-audit.sh --strict
fi

for shell_file in "${shell_files[@]}"; do
  run bash -n "$shell_file"
done

if [[ "$third_party_artifacts" -eq 1 ]]; then
  run bash scripts/ci/third-party-artifacts-audit.sh --strict
fi

test_runner="$(select_test_runner)"
echo "LOCAL_FAST_TEST_RUNNER=$test_runner"

run cargo fmt --all -- --check

if [[ "$mode" == "workspace" ]]; then
  run cargo clippy --all-targets --all-features -- -D warnings
  if [[ "$test_runner" == "nextest" ]]; then
    run cargo nextest run --profile ci --workspace
    run cargo test --workspace --doc
  else
    run cargo test --workspace
  fi
  echo "ok: local-fast workspace Rust gate passed"
  exit 0
fi

for package in "${packages[@]}"; do
  run cargo clippy -p "$package" --all-targets --all-features -- -D warnings
done

for package in "${packages[@]}"; do
  if [[ "$test_runner" == "nextest" ]]; then
    run cargo nextest run --profile ci -p "$package"
    if package_has_doctest "$package"; then
      run cargo test -p "$package" --doc
    fi
  else
    run cargo test -p "$package"
  fi
done

echo "ok: local-fast package checks passed (${#packages[@]} package(s))"
