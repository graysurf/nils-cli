#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  nils-cli-bump-version-tag-release --version X.Y.Z [options]

Options:
  --version X.Y.Z   Required. Accepts vX.Y.Z and normalizes to X.Y.Z.
  --skip-checks     Skip full lint/tests; refresh Cargo.lock then run locked cargo check.
  --ci-gate-main    Require CI gate on main; fail if gate conditions are not met.
  --skip-readme     Do not update README release tag examples.
  --skip-push       Do not push commit or tag to origin.
  --allow-dirty     Allow a dirty working tree.
  --force-tag       Delete existing local/remote tag before re-tagging.
  -h, --help        Show help.

Default behavior:
  Without --skip-checks/--ci-gate-main, the script first tries CI gate on main.
  If CI gate is unavailable, it falls back to full release checks.
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

note() {
  echo "info: $*" >&2
}

refresh_lockfile() {
  note "refreshing Cargo.lock for release changes"
  cargo generate-lockfile
}

verify_workspace_locked() {
  note "verifying workspace with cargo check --workspace --locked"
  cargo check --workspace --locked
}

refresh_lockfile_and_verify_locked() {
  refresh_lockfile
  verify_workspace_locked
}

sanitize_rust_build_env() {
  local wrapper="${RUSTC_WRAPPER:-}"
  if [[ -z "$wrapper" ]]; then
    return 0
  fi

  local wrapper_bin="$wrapper"
  if [[ "$wrapper" == */* ]]; then
    if [[ ! -x "$wrapper" ]]; then
      note "RUSTC_WRAPPER=${wrapper} is not executable; disabling it for release commands"
      unset RUSTC_WRAPPER
      return 0
    fi
  else
    if ! wrapper_bin="$(command -v "$wrapper" 2>/dev/null)"; then
      note "RUSTC_WRAPPER=${wrapper} is not available on PATH; disabling it for release commands"
      unset RUSTC_WRAPPER
      return 0
    fi
  fi

  local rustc_bin probe_output="" probe_summary=""
  rustc_bin="$(command -v rustc 2>/dev/null || true)"
  [[ -n "$rustc_bin" ]] || die "rustc is not available on PATH"

  if probe_output="$("$wrapper_bin" "$rustc_bin" -vV 2>&1)"; then
    return 0
  fi

  probe_summary="${probe_output%%$'\n'*}"
  note "RUSTC_WRAPPER=${wrapper} is not compatible with the active rustc; disabling it for release commands"
  if [[ -n "$probe_summary" ]]; then
    note "wrapper probe: ${probe_summary}"
  fi
  unset RUSTC_WRAPPER

  if [[ "$(basename "$wrapper_bin")" == "sccache" ]]; then
    export SCCACHE_DISABLE=1
    note "set SCCACHE_DISABLE=1 after disabling incompatible sccache wrapper"
  fi
}

refresh_third_party_artifacts_if_present() {
  local generator_script="scripts/generate-third-party-artifacts.sh"
  local artifacts=("THIRD_PARTY_LICENSES.md" "THIRD_PARTY_NOTICES.md")
  local tracked_count=0
  local artifact

  for artifact in "${artifacts[@]}"; do
    if git ls-files --error-unmatch "$artifact" >/dev/null 2>&1; then
      tracked_count=$((tracked_count + 1))
    fi
  done

  if [[ "$tracked_count" -eq 0 ]]; then
    return 0
  fi

  [[ -f "$generator_script" ]] \
    || die "tracked third-party artifacts require generator script: ${generator_script}"

  note "regenerating third-party artifacts for release changes"
  bash "$generator_script" --write
}

ci_gate_main_url=""
ci_gate_main_error=""

check_ci_gate_main() {
  ci_gate_main_url=""
  ci_gate_main_error=""

  if [[ -z "${current_branch:-}" || "${current_branch}" != "main" ]]; then
    ci_gate_main_error="current branch is '${current_branch:-detached}' (requires main)"
    return 10
  fi

  if ! command -v gh >/dev/null 2>&1; then
    ci_gate_main_error="gh is not available on PATH"
    return 11
  fi

  note "verifying CI status for origin/main"
  if ! git fetch origin main --quiet; then
    ci_gate_main_error="failed to fetch origin/main"
    return 12
  fi

  local head_sha origin_main_sha
  head_sha="$(git rev-parse --verify HEAD)"
  origin_main_sha="$(git rev-parse --verify origin/main)"
  if [[ "$head_sha" != "$origin_main_sha" ]]; then
    ci_gate_main_error="HEAD (${head_sha}) does not match origin/main (${origin_main_sha})"
    return 13
  fi

  local ci_run_json ci_run_result
  ci_run_json="$(gh run list --workflow ci.yml --branch main --event push --commit "$origin_main_sha" --limit 20 --json databaseId,status,conclusion,url,headSha 2>/dev/null)" \
    || {
      ci_gate_main_error="failed to query CI runs from GitHub"
      return 14
    }

  ci_run_result="$(
    python3 - "$origin_main_sha" "$ci_run_json" <<'PY'
from __future__ import annotations

import json
import sys

sha = sys.argv[1]
runs = json.loads(sys.argv[2])
if not runs:
    print(f"error:no CI run found for origin/main ({sha})")
    raise SystemExit(2)

run = runs[0]
run_head_sha = run.get("headSha")
status = run.get("status")
conclusion = run.get("conclusion")
url = run.get("url", "")

if run_head_sha and run_head_sha != sha:
    print(f"error:CI run SHA mismatch ({run_head_sha} != {sha}): {url}")
    raise SystemExit(5)
if status != "completed":
    print(f"error:CI run is not completed yet ({status}): {url}")
    raise SystemExit(3)
if conclusion != "success":
    print(f"error:CI run is not green ({conclusion}): {url}")
    raise SystemExit(4)

print(f"ok:{url}")
PY
  )" || {
    ci_gate_main_error="${ci_run_result#error:}"
    return 15
  }

  if [[ "$ci_run_result" != ok:* ]]; then
    ci_gate_main_error="unexpected CI gate result"
    return 16
  fi

  ci_gate_main_url="${ci_run_result#ok:}"
  return 0
}

version=""
skip_checks=0
ci_gate_main=0
skip_readme=0
skip_push=0
allow_dirty=0
force_tag=0

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --version)
      if [[ $# -lt 2 ]]; then
        die "--version requires a value"
      fi
      version="${2:-}"
      shift 2
      ;;
    --skip-checks)
      skip_checks=1
      shift
      ;;
    --ci-gate-main)
      ci_gate_main=1
      shift
      ;;
    --skip-readme)
      skip_readme=1
      shift
      ;;
    --skip-push)
      skip_push=1
      shift
      ;;
    --allow-dirty)
      allow_dirty=1
      shift
      ;;
    --force-tag)
      force_tag=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: ${1:-}"
      ;;
  esac
 done

if [[ -z "$version" ]]; then
  usage >&2
  exit 2
fi

if [[ "$version" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
  version="${BASH_REMATCH[1]}"
fi
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  die "invalid --version: ${version} (expected X.Y.Z or vX.Y.Z)"
fi

tag="v${version}"

for cmd in git python3 cargo semantic-commit git-scope; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    die "missing required command: ${cmd}"
  fi
done

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" ]]; then
  die "must run inside a git work tree"
fi

cd "$repo_root"

if [[ ! -f Cargo.toml ]]; then
  die "Cargo.toml not found in repo root"
fi

sanitize_rust_build_env

current_branch="$(git branch --show-current 2>/dev/null || true)"
if [[ -n "$current_branch" && "$current_branch" != "main" ]]; then
  note "current branch is '${current_branch}' (release tags are typically on main)"
fi

if [[ "$allow_dirty" -eq 0 ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    die "working tree is not clean; commit/stash changes or use --allow-dirty"
  fi
fi

if [[ "$skip_checks" -eq 0 ]]; then
  if [[ "$ci_gate_main" -eq 1 ]]; then
    if check_ci_gate_main; then
      note "main CI is green: ${ci_gate_main_url}"
      skip_checks=1
    else
      die "--ci-gate-main requirement failed: ${ci_gate_main_error}"
    fi
  else
    if check_ci_gate_main; then
      note "main CI is green: ${ci_gate_main_url}"
      note "using CI gate result; skipping full release checks"
      skip_checks=1
    else
      note "CI gate unavailable (${ci_gate_main_error}); running full release checks"
    fi
  fi
fi

python3 - "$version" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

version = sys.argv[1]

paths = [Path("Cargo.toml")] + sorted(Path("crates").glob("*/Cargo.toml"))
updated: list[str] = []
version_fields_found = 0
dep_fields_seen = 0


def extract_package_name(path: Path) -> str | None:
    section = None
    for line in path.read_text("utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped.strip("[]")
            continue
        if section == "package":
            match = re.match(r'\s*name\s*=\s*"([^"]+)"\s*$', line)
            if match:
                return match.group(1)
    return None


workspace_packages = {name for path in paths if (name := extract_package_name(path))}

for path in paths:
    text = path.read_text("utf-8")
    lines = text.splitlines()
    section = None
    out: list[str] = []
    changed = False

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped.strip("[]")
        if section in {"package", "workspace.package"}:
            match = re.match(r"(\s*version\s*=\s*)\"[^\"]+\"(.*)", line)
            if match:
                version_fields_found += 1
                new_line = f"{match.group(1)}\"{version}\"{match.group(2)}"
                if new_line != line:
                    line = new_line
                    changed = True

        dep_match = re.match(r'(\s*([A-Za-z0-9_.-]+)\s*=\s*\{)(.*)(\}\s*(?:#.*)?)$', line)
        if dep_match:
            dep_fields_seen += 1
            dep_key = dep_match.group(2).strip('"')
            body = dep_match.group(3)
            suffix = dep_match.group(4)
            package_match = re.search(r'\bpackage\s*=\s*"([^"]+)"', body)
            package_name = package_match.group(1) if package_match else dep_key

            if package_name in workspace_packages and re.search(r"\bpath\s*=", body):
                if re.search(r"\bversion\s*=", body):
                    new_body = re.sub(
                        r'(\bversion\s*=\s*)"[^"]+"',
                        rf'\1"{version}"',
                        body,
                        count=1,
                    )
                else:
                    path_match = re.search(r"\bpath\s*=", body)
                    if path_match:
                        idx = path_match.start()
                        new_body = body[:idx] + f'version = "{version}", ' + body[idx:]
                    else:
                        new_body = body

                if new_body != body:
                    line = f"{dep_match.group(1)}{new_body}{suffix}"
                    changed = True
        out.append(line)

    if changed:
        new_text = "\n".join(out)
        if text.endswith("\n"):
            new_text += "\n"
        path.write_text(new_text, "utf-8")
        updated.append(path.as_posix())

if not updated:
    if version_fields_found == 0 and dep_fields_seen == 0:
        print("error: no version fields found in Cargo manifests or dependency tables", file=sys.stderr)
        raise SystemExit(2)
    print("info: all manifest versions already set to target; continuing")
else:
    print("info: updated versions in:")
    for item in updated:
        print(f"- {item}")
PY

if [[ "$skip_readme" -eq 0 ]]; then
  if [[ -f README.md ]]; then
    python3 - "$version" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

version = sys.argv[1]
tag = f"v{version}"
path = Path("README.md")
text = path.read_text("utf-8")
lines = text.splitlines()
out: list[str] = []
updated = False

patterns = (
    "tag like `v",
    "git tag -a v",
    "git push origin v",
)
matched = False

for line in lines:
    if any(pat in line for pat in patterns):
        matched = True
        new_line = re.sub(r"v\d+\.\d+\.\d+", tag, line)
        if new_line != line:
            updated = True
        out.append(new_line)
    else:
        out.append(line)

if updated:
    new_text = "\n".join(out)
    if text.endswith("\n"):
        new_text += "\n"
    path.write_text(new_text, "utf-8")
elif not matched:
    print("warning: README release tag example not updated (pattern not found)", file=sys.stderr)
PY
  else
    note "README.md not found; skipping README update"
  fi
fi

checks_script="$repo_root/.agents/skills/nils-cli-verify-required-checks/scripts/nils-cli-verify-required-checks.sh"
if [[ "$skip_checks" -eq 0 ]]; then
  refresh_lockfile
  # Keep third-party artifacts aligned before strict audits (which include drift checks).
  refresh_third_party_artifacts_if_present

  if [[ ! -f "$checks_script" ]]; then
    die "missing checks script: $checks_script"
  fi
  checks_runner="${NILS_CLI_TEST_RUNNER:-nextest}"
  if [[ -z "${NILS_CLI_TEST_RUNNER:-}" ]]; then
    note "NILS_CLI_TEST_RUNNER not set; defaulting to nextest for release checks"
  fi
  NILS_CLI_TEST_RUNNER="$checks_runner" "$checks_script"
else
  refresh_lockfile_and_verify_locked
fi

# Re-run artifact generation after checks in case lockfile changed during the check flow.
refresh_third_party_artifacts_if_present

# Stage only the files this skill is expected to produce. Using `git add -A`
# would sweep in unrelated runtime state (e.g. `.claude/` session locks).
stage_paths=(Cargo.toml Cargo.lock)
for optional in README.md THIRD_PARTY_LICENSES.md THIRD_PARTY_NOTICES.md; do
  [[ -f "$optional" ]] && stage_paths+=("$optional")
done
for manifest in crates/*/Cargo.toml; do
  [[ -f "$manifest" ]] && stage_paths+=("$manifest")
done
git add -- "${stage_paths[@]}"

if git diff --cached --quiet; then
  die "no changes staged for commit"
fi

changed_files="$(git diff --cached --name-only)"

body_lines=()
body_lines+=("- Bump workspace and CLI crate versions to ${version}")
if [[ "$skip_readme" -eq 0 ]] && echo "$changed_files" | grep -qx "README.md"; then
  body_lines+=("- Update README release tag example to ${tag}")
fi
if echo "$changed_files" | grep -qx "Cargo.lock"; then
  body_lines+=("- Refresh Cargo.lock for workspace package versions")
fi
if echo "$changed_files" | grep -Eq "^(THIRD_PARTY_LICENSES\.md|THIRD_PARTY_NOTICES\.md)$"; then
  body_lines+=("- Regenerate third-party artifacts for updated lockfile inputs")
fi

{
  printf "chore(release): bump cli versions to %s\n\n" "$version"
  for line in "${body_lines[@]}"; do
    printf "%s\n" "$line"
  done
} | semantic-commit commit

if [[ "$skip_push" -eq 0 ]]; then
  git push origin HEAD
fi

if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
  if [[ "$force_tag" -eq 1 ]]; then
    git tag -d "$tag"
    if [[ "$skip_push" -eq 0 ]]; then
      git push origin ":refs/tags/${tag}"
    fi
  else
    die "tag already exists: ${tag} (use --force-tag to replace)"
  fi
fi

git tag -a "$tag" -m "$tag"

if [[ "$skip_push" -eq 0 ]]; then
  git push origin "$tag"
fi

note "release tag ${tag} created"
