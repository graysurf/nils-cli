#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  nils-cli-bump-version-tag-release --version X.Y.Z [options]

Options:
  --version X.Y.Z         Required. Accepts vX.Y.Z and normalizes to X.Y.Z.
  --full-checks           Run the full local audit stack (nils-cli-verify-required-checks.sh)
                          before commit; opt-in (slow). Default skips local audit and trusts CI.
  --skip-checks           Deprecated alias of the new default (locked cargo check only).
  --ci-gate-main          Require CI gate on main (pre-bump check): fail if the prior
                          origin/main commit's ci.yml run is not green.
  --skip-readme           Do not update README release tag examples.
  --skip-push             Do not push commit or tag to origin (also disables tap stage).
  --skip-ci-wait          Do not wait for ci.yml on the bump commit before the tap stage.
  --allow-dirty           Allow dirty release-managed files only.
  --force-tag             Delete existing local/remote tag before re-tagging.
  --tap-dir <path>        Path to homebrew-tap work tree (overrides env + convention).
  --skip-tap              Skip the homebrew-tap stage entirely.
  --skip-tap-wait         Do not wait for tap release.yml after pushing the prefix tag.
  --skip-tap-tag          Commit + push tap formula bump but skip the prefix tag.
  --from-tap              Resume mode: skip nils-cli stages 1-8 and run only the tap stage.
                          Requires --version and an existing v<version> tag in this repo.
  --tap-formula <name>    Formula basename to bump (default: nils-cli). Reserved for AWL et al.
  --skip-dev-clean        Do not clear ~/.local/nils-cli/bin after a successful release.
  --skip-local-brew-upgrade
                          Do not update/upgrade an installed local Homebrew formula after tap release.
  -h, --help              Show help.

Default behavior:
  Local checks are minimal: refresh Cargo.lock, regenerate third-party artifacts,
  then run `cargo check --workspace --locked`. The full audit stack (clippy, nextest,
  zsh completion, docs/audit scripts) runs on CI for every push — the bump commit
  itself triggers ci.yml, and the tap stage waits for that run to be green before
  bumping the homebrew formula. Use --full-checks to run the full audit locally
  (slow); use --skip-ci-wait to fire-and-forget without gating on the bump commit's
  ci.yml run.

  After tagging the nils-cli release, the tap stage is run automatically when:
    - --skip-push is NOT set, AND
    - --skip-tap is NOT set, AND
    - a tap work tree resolves via (--tap-dir | $NILS_CLI_HOMEBREW_TAP_DIR | <repo parent>/homebrew-tap).
  Otherwise the tap stage is skipped with a note.
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

note() {
  echo "info: $*" >&2
}

warn() {
  echo "warning: $*" >&2
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

release_managed_paths() {
  printf '%s\n' Cargo.toml Cargo.lock
  for optional in README.md THIRD_PARTY_LICENSES.md THIRD_PARTY_NOTICES.md; do
    if [[ -e "$optional" || -L "$optional" ]]; then
      printf '%s\n' "$optional"
    fi
  done
  for manifest in crates/*/Cargo.toml; do
    if [[ -f "$manifest" ]]; then
      printf '%s\n' "$manifest"
    fi
  done
}

assert_allow_dirty_only_release_managed() {
  if [[ -z "$(git status --porcelain=v1 --untracked-files=all)" ]]; then
    return 0
  fi

  local managed_file dirty_file unexpected
  managed_file="$(mktemp)"
  dirty_file="$(mktemp)"
  release_managed_paths | sort -u >"$managed_file"
  git status --porcelain=v1 --untracked-files=all \
    | sed -E 's/^.. //' \
    | sort -u >"$dirty_file"
  unexpected="$(comm -23 "$dirty_file" "$managed_file" || true)"
  rm -f "$managed_file" "$dirty_file"

  if [[ -n "$unexpected" ]]; then
    die "--allow-dirty only permits release-managed paths; commit/stash these first: ${unexpected//$'\n'/, }"
  fi
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

# === Tap helpers =============================================================

# Echo a tap work tree path, or return non-zero with an error on stderr.
# Resolution order: explicit flag > env var > convention <repo parent>/homebrew-tap.
resolve_tap_dir() {
  local explicit="$1"
  local repo_parent="$2"

  local candidate=""
  local source=""
  if [[ -n "$explicit" ]]; then
    candidate="$explicit"
    source="--tap-dir"
  elif [[ -n "${NILS_CLI_HOMEBREW_TAP_DIR:-}" ]]; then
    candidate="$NILS_CLI_HOMEBREW_TAP_DIR"
    source="\$NILS_CLI_HOMEBREW_TAP_DIR"
  elif [[ -n "$repo_parent" && -d "${repo_parent}/homebrew-tap" ]]; then
    candidate="${repo_parent}/homebrew-tap"
    source="convention"
  fi

  if [[ -z "$candidate" ]]; then
    return 1
  fi
  if ! candidate="$(cd "$candidate" 2>/dev/null && pwd)"; then
    echo "tap directory does not exist: $candidate (source: $source)" >&2
    return 1
  fi
  if ! git -C "$candidate" rev-parse --show-toplevel >/dev/null 2>&1; then
    echo "tap directory is not a git work tree: $candidate (source: $source)" >&2
    return 1
  fi

  echo "$candidate"
}

# Extract artifact origin "<owner>/<repo>" from a homebrew formula's first URL.
parse_formula_origin() {
  local formula_path="$1"
  python3 - "$formula_path" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text("utf-8")
match = re.search(
    r'url\s+"https://github\.com/([^/"]+/[^/"]+)/releases/download/v[0-9.]+/',
    text,
)
if not match:
    print("error: no artifact origin URL found in formula", file=sys.stderr)
    raise SystemExit(2)
print(match.group(1))
PY
}

# Wait for a workflow run on a given commit/tag-ref to finish.
# Args: repo, workflow_filename, head_ref (tag name or commit sha), max_seconds
wait_for_release_run() {
  local repo="$1"
  local workflow="$2"
  local head_ref="$3"
  local max_seconds="${4:-1200}"

  command -v gh >/dev/null 2>&1 || die "gh is required to wait for ${workflow} runs"

  local deadline=$((SECONDS + max_seconds))
  local run_id=""
  local last_status=""
  local conclusion=""
  local url=""

  while (( SECONDS < deadline )); do
    local runs_json
    runs_json="$(gh -R "$repo" run list --workflow "$workflow" --limit 20 \
      --json databaseId,status,conclusion,url,headBranch,headSha 2>/dev/null)" \
      || { sleep 10; continue; }

    local match
    match="$(
      python3 - "$head_ref" "$runs_json" <<'PY'
from __future__ import annotations

import json
import sys

needle = sys.argv[1]
runs = json.loads(sys.argv[2])
for run in runs:
    if run.get("headBranch") == needle or run.get("headSha", "").startswith(needle):
        print(json.dumps({
            "id": run.get("databaseId"),
            "status": run.get("status"),
            "conclusion": run.get("conclusion"),
            "url": run.get("url"),
        }))
        break
PY
    )"

    if [[ -z "$match" ]]; then
      sleep 15
      continue
    fi

    run_id="$(python3 -c "import json,sys;print(json.loads(sys.argv[1])['id'])" "$match")"
    last_status="$(python3 -c "import json,sys;print(json.loads(sys.argv[1])['status'] or '')" "$match")"
    conclusion="$(python3 -c "import json,sys;print(json.loads(sys.argv[1])['conclusion'] or '')" "$match")"
    url="$(python3 -c "import json,sys;print(json.loads(sys.argv[1])['url'] or '')" "$match")"

    if [[ "$last_status" == "completed" ]]; then
      if [[ "$conclusion" == "success" ]]; then
        note "${repo} ${workflow} run ${run_id} completed: ${url}"
        return 0
      fi
      die "${repo} ${workflow} run ${run_id} ended with conclusion='${conclusion}': ${url}"
    fi

    note "waiting for ${repo} ${workflow} run ${run_id} (${last_status:-pending}): ${url}"
    sleep 20
  done

  die "timed out after ${max_seconds}s waiting for ${workflow} on ${repo} for ${head_ref}"
}

# Fetch sha256 hex for the published artifact tarball; echoes the hex.
# Args: artifact_origin (owner/repo), version, arch
fetch_artifact_sha256() {
  local origin="$1"
  local version="$2"
  local arch="$3"

  local url="https://github.com/${origin}/releases/download/v${version}/nils-cli-v${version}-${arch}.tar.gz.sha256"
  local body
  body="$(curl -fsSL "$url" 2>/dev/null)" \
    || die "failed to fetch sha256 sidecar: $url"
  # Sidecar format: "<hex>  dist/<filename>" — take first token only.
  local hex
  hex="$(echo "$body" | awk 'NR==1 {print $1}')"
  if ! [[ "$hex" =~ ^[0-9a-f]{64}$ ]]; then
    die "invalid sha256 hex from $url: '${hex}'"
  fi
  echo "$hex"
}

# In-place rewrite of nils-cli formula URL + sha256 lines.
# Args: formula_path, version, sha_aarch64_darwin, sha_x86_64_darwin,
#       sha_aarch64_linux, sha_x86_64_linux
update_formula_inplace() {
  local formula_path="$1"
  local version="$2"
  local sha_a_d="$3"
  local sha_x_d="$4"
  local sha_a_l="$5"
  local sha_x_l="$6"

  python3 - \
    "$formula_path" "$version" \
    "$sha_a_d" "$sha_x_d" "$sha_a_l" "$sha_x_l" \
    <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

(_, formula_path, version,
 sha_a_d, sha_x_d, sha_a_l, sha_x_l) = sys.argv

sha_map = {
    "aarch64-apple-darwin": sha_a_d,
    "x86_64-apple-darwin": sha_x_d,
    "aarch64-unknown-linux-gnu": sha_a_l,
    "x86_64-unknown-linux-gnu": sha_x_l,
}

path = Path(formula_path)
text = path.read_text("utf-8")
lines = text.splitlines()
out: list[str] = []
last_arch: str | None = None
url_pattern = re.compile(
    r'^(?P<indent>\s*)url\s+"https://github\.com/(?P<origin>[^/"]+/[^/"]+)'
    r'/releases/download/v[0-9.]+/nils-cli-v[0-9.]+-(?P<arch>[a-z0-9_-]+)\.tar\.gz"\s*$'
)
sha_pattern = re.compile(r'^(?P<indent>\s*)sha256\s+"[0-9a-f]+"\s*$')

archs_seen: set[str] = set()
for line in lines:
    url_match = url_pattern.match(line)
    if url_match:
        arch = url_match.group("arch")
        if arch not in sha_map:
            print(f"error: unknown arch in formula URL: {arch}", file=sys.stderr)
            raise SystemExit(2)
        last_arch = arch
        archs_seen.add(arch)
        new_line = (
            f'{url_match.group("indent")}url '
            f'"https://github.com/{url_match.group("origin")}/releases/download/'
            f'v{version}/nils-cli-v{version}-{arch}.tar.gz"'
        )
        out.append(new_line)
        continue
    sha_match = sha_pattern.match(line)
    if sha_match and last_arch is not None:
        new_line = (
            f'{sha_match.group("indent")}sha256 "{sha_map[last_arch]}"'
        )
        out.append(new_line)
        last_arch = None
        continue
    out.append(line)

missing = sorted(set(sha_map) - archs_seen)
if missing:
    print(
        f"error: formula does not reference all expected archs; missing: {missing}",
        file=sys.stderr,
    )
    raise SystemExit(2)

new_text = "\n".join(out)
if text.endswith("\n"):
    new_text += "\n"
if new_text == text:
    print("info: formula already at target version + sha256; no edit needed")
    raise SystemExit(7)
path.write_text(new_text, "utf-8")
print("info: formula updated for v" + version)
PY
}

# Run the tap stage end-to-end. Caller must have validated source-side push.
run_tap_stage() {
  local tap_dir="$1"
  local source_repo_slug="$2"
  local version="$3"
  local tag="v${version}"
  local tap_formula="${4:-nils-cli}"
  local skip_tap_tag="$5"
  local skip_tap_wait="$6"

  local formula_path="${tap_dir}/Formula/${tap_formula}.rb"
  if [[ ! -f "$formula_path" ]]; then
    die "tap formula not found: $formula_path"
  fi

  for cmd in curl ruby; do
    command -v "$cmd" >/dev/null 2>&1 \
      || die "tap stage requires '${cmd}' on PATH"
  done

  note "tap stage starting (tap_dir=${tap_dir}, formula=${tap_formula})"

  if [[ -n "$(git -C "$tap_dir" status --porcelain)" ]]; then
    die "tap work tree is not clean: ${tap_dir} (commit/stash before re-running)"
  fi

  local tap_branch
  tap_branch="$(git -C "$tap_dir" branch --show-current 2>/dev/null || true)"
  if [[ -z "$tap_branch" || "$tap_branch" != "main" ]]; then
    die "tap is on branch '${tap_branch:-detached}' (must be on main)"
  fi

  note "fetching tap origin/main (--no-tags to avoid stale-tag clobber)"
  git -C "$tap_dir" fetch --no-tags origin main --quiet \
    || die "failed to fetch tap origin/main"
  git -C "$tap_dir" merge --ff-only origin/main \
    || die "tap main is not fast-forward to origin/main; resolve manually"

  # 1) Wait for source release.yml so artifacts are guaranteed available.
  note "waiting for ${source_repo_slug} release.yml on tag ${tag}"
  wait_for_release_run "$source_repo_slug" "release.yml" "$tag" "${NILS_CLI_RELEASE_WAIT_SECONDS:-1200}"

  # 2) Parse artifact origin from existing formula (no hardcoded owner/repo).
  local artifact_origin
  artifact_origin="$(parse_formula_origin "$formula_path")" \
    || die "failed to parse artifact origin from $formula_path"
  note "artifact origin: ${artifact_origin}"

  # 3) Fetch sha256 sidecars for all 4 platforms.
  note "fetching sha256 sidecars from ${artifact_origin} v${version}"
  local sha_a_d sha_x_d sha_a_l sha_x_l
  sha_a_d="$(fetch_artifact_sha256 "$artifact_origin" "$version" "aarch64-apple-darwin")"
  sha_x_d="$(fetch_artifact_sha256 "$artifact_origin" "$version" "x86_64-apple-darwin")"
  sha_a_l="$(fetch_artifact_sha256 "$artifact_origin" "$version" "aarch64-unknown-linux-gnu")"
  sha_x_l="$(fetch_artifact_sha256 "$artifact_origin" "$version" "x86_64-unknown-linux-gnu")"

  # 4) Edit formula in place (idempotent: exit code 7 = already at target).
  local update_rc=0
  (
    cd "$tap_dir"
    update_formula_inplace "Formula/${tap_formula}.rb" \
      "$version" "$sha_a_d" "$sha_x_d" "$sha_a_l" "$sha_x_l"
  ) || update_rc=$?

  local formula_changed=1
  if [[ "$update_rc" -eq 7 ]]; then
    formula_changed=0
    note "formula already at v${version}; skipping commit"
  elif [[ "$update_rc" -ne 0 ]]; then
    die "formula update failed (exit ${update_rc})"
  fi

  # 5) Validate (always — cheap and catches drift even when no edit).
  note "validating formula syntax + style"
  ruby -c "$formula_path" >/dev/null \
    || die "ruby -c failed on $formula_path"
  if command -v brew >/dev/null 2>&1; then
    HOMEBREW_NO_AUTO_UPDATE=1 brew style "$formula_path" \
      || die "brew style failed on $formula_path"
  else
    warn "brew not on PATH; skipping 'brew style' validation"
  fi

  # 6) Commit + push when formula changed.
  if [[ "$formula_changed" -eq 1 ]]; then
    note "committing tap formula bump"
    (
      cd "$tap_dir"
      git add "Formula/${tap_formula}.rb"
      printf 'chore(formula): bump %s to v%s\n\n- Update macOS/Linux URLs and sha256 to v%s release artifacts.\n' \
        "$tap_formula" "$version" "$version" \
        | semantic-commit commit
      git push origin HEAD
    )
  else
    note "no formula changes to commit"
  fi

  # 7) Push prefix tag to trigger tap release.yml.
  local prefix_tag="${tap_formula}-v${version}"
  if [[ "$skip_tap_tag" -eq 1 ]]; then
    note "--skip-tap-tag set; not creating ${prefix_tag}"
    return 0
  fi

  if git -C "$tap_dir" rev-parse -q --verify "refs/tags/${prefix_tag}" >/dev/null; then
    note "tap tag ${prefix_tag} already exists locally; ensuring it is pushed"
  else
    note "creating tap tag ${prefix_tag}"
    git -C "$tap_dir" tag -a "$prefix_tag" -m "${tap_formula} v${version}"
  fi

  # Push tag (--no-verify-signatures harmless; idempotent if already pushed).
  if ! git -C "$tap_dir" push origin "$prefix_tag" 2>&1; then
    warn "tap tag push reported non-zero; checking if already on origin"
    if ! git -C "$tap_dir" ls-remote --tags origin "$prefix_tag" | grep -q "$prefix_tag"; then
      die "failed to push tap tag ${prefix_tag}"
    fi
  fi

  if [[ "$skip_tap_wait" -eq 1 ]]; then
    note "--skip-tap-wait set; not waiting for tap release.yml"
    return 0
  fi

  local tap_repo_slug
  tap_repo_slug="$(git -C "$tap_dir" remote get-url origin 2>/dev/null \
    | sed -E 's#(git@github\.com:|https://github\.com/)([^/]+/[^/.]+)(\.git)?$#\2#')"
  if [[ -z "$tap_repo_slug" || "$tap_repo_slug" == *"/"*"/"* ]]; then
    warn "could not determine tap repo slug; skipping tap release.yml wait"
    return 0
  fi

  note "waiting for ${tap_repo_slug} release.yml on tag ${prefix_tag}"
  wait_for_release_run "$tap_repo_slug" "release.yml" "$prefix_tag" "${NILS_CLI_TAP_WAIT_SECONDS:-1200}"
  note "tap release.yml green for ${prefix_tag}"
}

clean_dev_install() {
  local skip="$1"
  if [[ "$skip" -eq 1 ]]; then
    note "--skip-dev-clean set; leaving ~/.local/nils-cli/bin untouched"
    return 0
  fi
  local dev_bin="${HOME}/.local/nils-cli/bin"
  if [[ ! -d "$dev_bin" ]]; then
    return 0
  fi
  if [[ -z "$(ls -A "$dev_bin" 2>/dev/null)" ]]; then
    return 0
  fi
  note "clearing dev install at ${dev_bin} so brew copies take precedence"
  find "$dev_bin" -mindepth 1 -delete
}

upgrade_local_brew_install() {
  local skip="$1"
  local formula="$2"
  local target_version="$3"

  if [[ "$skip" -eq 1 ]]; then
    note "--skip-local-brew-upgrade set; leaving local Homebrew install untouched"
    return 0
  fi

  if ! command -v brew >/dev/null 2>&1; then
    note "brew not on PATH; skipping local Homebrew upgrade"
    return 0
  fi

  if ! brew list --formula "$formula" >/dev/null 2>&1; then
    note "Homebrew formula ${formula} is not installed locally; skipping local upgrade"
    return 0
  fi

  note "updating Homebrew taps before local ${formula} upgrade"
  brew update

  note "upgrading local Homebrew formula ${formula} to v${target_version}"
  brew upgrade "$formula"

  local installed_version=""
  installed_version="$(brew list --versions "$formula" 2>/dev/null | awk '{print $2; exit}')"
  if [[ "$installed_version" != "$target_version" ]]; then
    die "local Homebrew formula ${formula} is at ${installed_version:-unknown}, expected ${target_version}"
  fi
  note "local Homebrew formula ${formula} is at ${target_version}"
}

# === Argument parsing ========================================================

version=""
full_checks=0
skip_checks=0  # backward-compat alias of new default; tracked for usage notes only
ci_gate_main=0
skip_readme=0
skip_push=0
skip_ci_wait=0
allow_dirty=0
force_tag=0
tap_dir_arg=""
skip_tap=0
skip_tap_wait=0
skip_tap_tag=0
from_tap=0
tap_formula="nils-cli"
skip_dev_clean=0
skip_local_brew_upgrade=0

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --version)
      if [[ $# -lt 2 ]]; then
        die "--version requires a value"
      fi
      version="${2:-}"
      shift 2
      ;;
    --full-checks)
      full_checks=1
      shift
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
    --skip-ci-wait)
      skip_ci_wait=1
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
    --tap-dir)
      if [[ $# -lt 2 ]]; then
        die "--tap-dir requires a value"
      fi
      tap_dir_arg="${2:-}"
      shift 2
      ;;
    --skip-tap)
      skip_tap=1
      shift
      ;;
    --skip-tap-wait)
      skip_tap_wait=1
      shift
      ;;
    --skip-tap-tag)
      skip_tap_tag=1
      shift
      ;;
    --from-tap)
      from_tap=1
      shift
      ;;
    --tap-formula)
      if [[ $# -lt 2 ]]; then
        die "--tap-formula requires a value"
      fi
      tap_formula="${2:-}"
      shift 2
      ;;
    --skip-dev-clean)
      skip_dev_clean=1
      shift
      ;;
    --skip-local-brew-upgrade)
      skip_local_brew_upgrade=1
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

repo_parent="$(cd "$repo_root/.." 2>/dev/null && pwd || true)"
source_repo_slug="$(git -C "$repo_root" remote get-url origin 2>/dev/null \
  | sed -E 's#(git@github\.com:|https://github\.com/)([^/]+/[^/.]+)(\.git)?$#\2#' \
  || true)"

# === --from-tap shortcut: skip stages 1-8 ====================================

if [[ "$from_tap" -eq 1 ]]; then
  if [[ "$skip_tap" -eq 1 ]]; then
    die "--from-tap and --skip-tap are mutually exclusive"
  fi

  if ! git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
    die "--from-tap requires existing local tag ${tag}; create the source-side release first"
  fi

  if [[ -z "$source_repo_slug" || "$source_repo_slug" == *"/"*"/"* ]]; then
    die "--from-tap could not determine source repo slug from origin remote"
  fi

  tap_dir="$(resolve_tap_dir "$tap_dir_arg" "$repo_parent")" \
    || die "tap directory could not be resolved (use --tap-dir or NILS_CLI_HOMEBREW_TAP_DIR)"

  run_tap_stage \
    "$tap_dir" \
    "$source_repo_slug" \
    "$version" \
    "$tap_formula" \
    "$skip_tap_tag" \
    "$skip_tap_wait"
  clean_dev_install "$skip_dev_clean"
  upgrade_local_brew_install "$skip_local_brew_upgrade" "$tap_formula" "$version"
  exit 0
fi

# === Standard flow: stages 1-8 (existing behavior) ==========================

if [[ "$allow_dirty" -eq 0 ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    die "working tree is not clean; commit/stash changes or use --allow-dirty"
  fi
else
  assert_allow_dirty_only_release_managed
fi

if [[ "$ci_gate_main" -eq 1 ]]; then
  if check_ci_gate_main; then
    note "main CI is green: ${ci_gate_main_url}"
  else
    die "--ci-gate-main requirement failed: ${ci_gate_main_error}"
  fi
fi

if [[ "$skip_checks" -eq 1 && "$full_checks" -eq 0 ]]; then
  note "--skip-checks is a deprecated alias of the new default (locked cargo check only); ignoring"
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
refresh_lockfile
# Keep third-party artifacts aligned with the new lockfile; CI re-audits drift on the bump commit.
refresh_third_party_artifacts_if_present

if [[ "$full_checks" -eq 1 ]]; then
  if [[ ! -f "$checks_script" ]]; then
    die "missing checks script: $checks_script"
  fi
  checks_runner="${NILS_CLI_TEST_RUNNER:-nextest}"
  if [[ -z "${NILS_CLI_TEST_RUNNER:-}" ]]; then
    note "NILS_CLI_TEST_RUNNER not set; defaulting to nextest for --full-checks"
  fi
  NILS_CLI_TEST_RUNNER="$checks_runner" "$checks_script"
else
  verify_workspace_locked
fi

# Re-run artifact generation after checks in case lockfile changed during the check flow.
refresh_third_party_artifacts_if_present

# Stage only the files this skill is expected to produce. Using `git add -A`
# would sweep in unrelated runtime state (e.g. `.claude/` session locks).
stage_paths=()
while IFS= read -r release_path; do
  stage_paths+=("$release_path")
done < <(release_managed_paths)
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

# === Wait for ci.yml on the bump commit (default safety gate) ===============

if [[ "$skip_push" -eq 0 && "$skip_ci_wait" -eq 0 ]]; then
  if ! command -v gh >/dev/null 2>&1; then
    warn "gh not on PATH; skipping ci.yml wait on bump commit"
  elif [[ -z "$source_repo_slug" || "$source_repo_slug" == *"/"*"/"* ]]; then
    warn "could not determine source repo slug; skipping ci.yml wait"
  else
    bump_sha="$(git rev-parse --verify HEAD)"
    note "waiting for ${source_repo_slug} ci.yml on bump commit ${bump_sha}"
    wait_for_release_run "$source_repo_slug" "ci.yml" "$bump_sha" "${NILS_CLI_CI_WAIT_SECONDS:-1800}"
  fi
fi

# === Tap stage (auto-skipped when --skip-push or unable to resolve tap) =====

if [[ "$skip_push" -eq 1 ]]; then
  note "--skip-push set; tap stage skipped (no remote tag to wait on)"
  exit 0
fi

if [[ "$skip_tap" -eq 1 ]]; then
  note "--skip-tap set; tap stage skipped"
  exit 0
fi

tap_dir=""
if ! tap_dir="$(resolve_tap_dir "$tap_dir_arg" "$repo_parent" 2>&1)"; then
  resolve_err="$tap_dir"
  if [[ -n "$tap_dir_arg" || -n "${NILS_CLI_HOMEBREW_TAP_DIR:-}" ]]; then
    die "tap stage requested but tap directory invalid: ${resolve_err}"
  fi
  note "tap stage skipped (no tap configured; set NILS_CLI_HOMEBREW_TAP_DIR or pass --tap-dir to enable)"
  exit 0
fi

if [[ -z "$source_repo_slug" || "$source_repo_slug" == *"/"*"/"* ]]; then
  die "tap stage cannot determine source repo slug from origin remote"
fi

run_tap_stage \
  "$tap_dir" \
  "$source_repo_slug" \
  "$version" \
  "$tap_formula" \
  "$skip_tap_tag" \
  "$skip_tap_wait"

clean_dev_install "$skip_dev_clean"
upgrade_local_brew_install "$skip_local_brew_upgrade" "$tap_formula" "$version"
