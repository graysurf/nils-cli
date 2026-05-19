#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/cli-output-contract-lint.sh [--strict]

Enforces the workspace CLI output contract (docs/specs/cli-output-contract-v1.md)
on Rust sources. Fails on three classes of regression:

  (a) New `--json` boolean clap flag without `hide = true`.
      The contract requires `--json` to be a hidden alias for `--format json`.
      Existing pre-migration sites are tracked in JSON_ALLOWED_FILES below.

  (b) Inline `process::exit(1|2)` / `std::process::exit(1|2)` literals in
      `crates/*/src/main.rs`. Usage errors must call
      `nils_common::cli_contract::exit::USAGE` (64). Runtime errors must call
      `exit::RUNTIME`. Inline 1/2 literals indicate drift from the contract.

  (c) `#[serde(rename_all = "camelCase")]` or `#[serde(rename = "<camelCase>")]`
      attributes outside the documented alias allowlists. CLI envelopes are
      snake_case; the only documented camelCase aliases live in
      semantic-commit's staged-context v2 schema, AWP records, and the
      api-test/api-testing-core JUnit-compatible suite schemas.

Options:
  --strict   Treat warnings as hard failures (no-op today; reserved)
  -h, --help Show this help

Exit codes:
  0  no contract drift
  1  contract drift detected
  2  usage error
USAGE
}

strict=0
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --strict)
      strict=1
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

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

if ! command -v rg >/dev/null 2>&1; then
  echo "error: ripgrep (rg) is required on PATH" >&2
  exit 2
fi

# ----------------------------------------------------------------------------
# Allowlists (tighten over time; never widen without an explicit plan entry)
# ----------------------------------------------------------------------------

# Files with grandfathered `--json` boolean flags lacking `hide = true`.
# These predate the contract's hidden-alias requirement; remove once their
# one-minor-cycle migration completes.
declare -a JSON_ALLOWED_FILES=(
  "crates/codex-cli/src/cli.rs"
  "crates/gemini-cli/src/cli.rs"
  "crates/image-processing/src/cli.rs"
  "crates/plan-issue-cli/src/cli.rs"
)

# Files where camelCase serde renames are intentional (non-CLI-envelope
# persistent records: AWP, JUnit-compat suite schemas, documented v2 alias).
declare -a CAMEL_ALLOWED_FILES=(
  "crates/agent-workflow-primitives/src/repo_retro.rs"
  "crates/api-test/src/suite_schema.rs"
  "crates/api-testing-core/src/suite/results.rs"
  "crates/api-testing-core/src/suite/schema.rs"
  "crates/semantic-commit/src/staged_context.rs"
)

is_in_array() {
  local needle="$1"
  shift
  local item
  for item in "$@"; do
    if [[ "$item" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

declare -a errors=()

record_error() {
  errors+=("$1")
}

# ----------------------------------------------------------------------------
# Check (a): --json bool flag without hide = true
# ----------------------------------------------------------------------------

# Find every Rust source containing a `json: bool` field (clap arg pattern).
declare -a json_field_files=()
while IFS= read -r path; do
  [[ -n "$path" ]] || continue
  json_field_files+=("$path")
done < <(rg -l -t rust -e '^\s*pub json:\s*bool' -e '^\s*json:\s*bool' crates/ 2>/dev/null || true)

for path in "${json_field_files[@]}"; do
  if is_in_array "$path" "${JSON_ALLOWED_FILES[@]}"; then
    continue
  fi

  # For each `json: bool` field, look at the 4 lines preceding it for
  # `#[arg(...)]` attributes containing `hide = true`. The pattern handles
  # both `long` (implicit `--json` from field name) and `long = "json"`.
  if ! awk '
    BEGIN { violations = 0 }
    {
      lines[NR] = $0
    }
    END {
      for (i = 1; i <= NR; i++) {
        if (lines[i] ~ /^[[:space:]]*(pub[[:space:]]+)?json[[:space:]]*:[[:space:]]*bool/) {
          # Walk backward to find the preceding #[arg(...)] block.
          attr_start = 0
          for (j = i - 1; j >= 1 && j >= i - 8; j--) {
            if (lines[j] ~ /#\[arg\(/) {
              attr_start = j
              break
            }
            if (lines[j] ~ /^[[:space:]]*$/) {
              break
            }
            if (lines[j] !~ /^[[:space:]]*\/\// && lines[j] !~ /,$/ && lines[j] !~ /^[[:space:]]*#\[/) {
              break
            }
          }
          if (attr_start == 0) {
            # No attribute found — not a clap flag, skip.
            continue
          }
          block = ""
          for (k = attr_start; k <= i; k++) {
            block = block lines[k] "\n"
          }
          if (block !~ /hide[[:space:]]*=[[:space:]]*true/) {
            violations++
            printf("%d\n", i)
          }
        }
      }
      exit (violations == 0 ? 0 : 1)
    }
  ' "$path" >/tmp/cli-contract-lint-json-$$ 2>/dev/null; then
    while IFS= read -r lineno; do
      [[ -n "$lineno" ]] || continue
      record_error "(a) --json bool flag without 'hide = true' at $path:$lineno (add 'hide = true' or update JSON_ALLOWED_FILES with justification)"
    done </tmp/cli-contract-lint-json-$$
  fi
  rm -f /tmp/cli-contract-lint-json-$$
done

# ----------------------------------------------------------------------------
# Check (b): inline process::exit(1|2) in crates/*/src/main.rs
# ----------------------------------------------------------------------------

# `process::exit(1)` and `process::exit(2)` are forbidden in entrypoints;
# usage must call `exit::USAGE`, runtime must call `exit::RUNTIME`.
exit_hits="$(rg -n --type rust -e 'process::exit\(\s*[12]\s*\)' crates/*/src/main.rs 2>/dev/null || true)"
if [[ -n "$exit_hits" ]]; then
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_error "(b) inline process::exit(1|2) literal at $line (use nils_common::cli_contract::exit::{USAGE,RUNTIME})"
  done <<<"$exit_hits"
fi

# ----------------------------------------------------------------------------
# Check (c): camelCase serde attrs outside allowlist
# ----------------------------------------------------------------------------

# Collect every Rust source touching a camelCase serde rename.
declare -a camel_files=()
while IFS= read -r path; do
  [[ -n "$path" ]] || continue
  camel_files+=("$path")
done < <(rg -l --type rust -e 'rename_all[[:space:]]*=[[:space:]]*"camelCase"' -e 'rename[[:space:]]*=[[:space:]]*"[a-z]+[A-Z]' crates/ 2>/dev/null || true)

for path in "${camel_files[@]}"; do
  if is_in_array "$path" "${CAMEL_ALLOWED_FILES[@]}"; then
    continue
  fi

  hits="$(rg -n --type rust -e 'rename_all[[:space:]]*=[[:space:]]*"camelCase"' -e 'rename[[:space:]]*=[[:space:]]*"[a-z]+[A-Z]' "$path" 2>/dev/null || true)"
  if [[ -n "$hits" ]]; then
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      record_error "(c) camelCase serde rename outside documented aliases at $path:$line (snake_case is the envelope contract; update CAMEL_ALLOWED_FILES if intentional)"
    done <<<"$hits"
  fi
done

# ----------------------------------------------------------------------------
# Report
# ----------------------------------------------------------------------------

if [[ ${#errors[@]} -gt 0 ]]; then
  for err in "${errors[@]}"; do
    echo "FAIL: $err"
  done
  echo "FAIL: cli output contract lint (strict=$strict, errors=${#errors[@]})"
  exit 1
fi

echo "PASS: cli output contract lint (strict=$strict)"
