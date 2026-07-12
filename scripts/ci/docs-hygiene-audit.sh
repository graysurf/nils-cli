#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  docs-hygiene-audit.sh [--strict]

Checks documentation hygiene policy:
  - known transient development records are removed and not referenced from active docs
  - crate docs indexes avoid unexpected deep links to root docs
  - duplicate markdown payloads are not present across active docs trees
  - legacy-removal guardrails stay enforced for docs and runtime surfaces

Options:
  --strict   Treat warnings as hard failures
  -h, --help Show this help
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
  echo "error: missing required tool on PATH: rg" >&2
  exit 2
fi

# Ripgrep exit 1 means a successful scan with no matches. Any larger status is
# an execution failure and must abort the audit instead of becoming a false
# green through an unconditional `|| true`.
rg_scan() {
  local status
  if rg "$@"; then
    return 0
  else
    status=$?
  fi
  if [[ "$status" -eq 1 ]]; then
    return 0
  fi
  echo "error: ripgrep scan failed (exit $status)" >&2
  return "$status"
}

# Production audits fail closed when an explicit scan target is missing.
# Black-box tests that intentionally build partial repositories must opt in to
# the narrowly named fixture relaxation.
rg_scan_existing() {
  local -a rg_args=()
  local -a paths=()
  local reading_paths=0
  local arg
  for arg in "$@"; do
    if [[ "$arg" == "--audit-paths" ]]; then
      reading_paths=1
    elif [[ "$reading_paths" -eq 1 ]]; then
      if [[ -e "$arg" ]]; then
        paths+=("$arg")
      elif [[ "${DOCS_HYGIENE_TEST_ALLOW_MISSING_TARGETS:-0}" != "1" ]]; then
        echo "error: missing required audit path: $arg" >&2
        return 2
      fi
    else
      rg_args+=("$arg")
    fi
  done
  if [[ ${#paths[@]} -eq 0 ]]; then
    return 0
  fi
  rg_scan "${rg_args[@]}" "${paths[@]}"
}

# Required audit globs must remain literal when unmatched so the helper can
# reject them. Do not inherit nullglob through an exported BASHOPTS value.
shopt -u nullglob

declare -a errors=()
declare -a warnings=()

record_issue() {
  local level="$1"
  local message="$2"
  if [[ "$level" == "error" || "$strict" -eq 1 ]]; then
    errors+=("$message")
  else
    warnings+=("$message")
  fi
}

declare -a removed_transient_docs=(
  "docs/reports/codex-gemini-doc-audit.md"
  "docs/reports/completion-coverage-matrix.md"
  "docs/plans/codex-gemini-core-merge-plan.md"
  "docs/plans/markdown-gh-handling-audit-remediation-plan.md"
  "docs/plans/repo-code-drift-followup-tracker.md"
  "docs/plans/repo-docs-cleanup-and-alignment-plan.md"
  "docs/plans/third-party-licenses-notices-release-packaging-plan.md"
  "docs/runbooks/image-processing-llm-svg.md"
  "docs/runbooks/wrappers-mode-usage.md"
  "docs/specs/markdown-github-handling-audit-v1.md"
  "crates/plan-issue/docs/specs/plan-issue-contract-v1.md"
  "crates/plan-tooling/docs/runbooks/split-prs-migration.md"
  "crates/api-test/docs/runbooks/api-test-websocket-adoption.md"
  "crates/api-websocket/docs/runbooks/api-websocket-rollout.md"
  "crates/memo/docs/runbooks/memo-rollout.md"
)

for path in "${removed_transient_docs[@]}"; do
  if [[ -e "$path" ]]; then
    record_issue error "transient doc must remain removed: $path"
  fi
done

declare -a reference_roots=(
  "README.md"
  "DEVELOPMENT.md"
  "AGENTS.md"
  "docs/runbooks"
  "docs/specs"
  "docs/reports"
  "crates"
)

declare -a existing_reference_roots=()
for root in "${reference_roots[@]}"; do
  if [[ -e "$root" ]]; then
    existing_reference_roots+=("$root")
  fi
done

for path in "${removed_transient_docs[@]}"; do
  refs=""
  if [[ ${#existing_reference_roots[@]} -gt 0 ]]; then
    refs="$(rg_scan -n --fixed-strings "$path" "${existing_reference_roots[@]}" \
      -g '!**/docs/plans/**' \
      -g '!docs/specs/workspace-doc-retention-matrix-v1.md' \
      -g '!**/tests/**' \
      -g '!**/target/**')"
  fi
  if [[ -n "$refs" ]]; then
    record_issue error "stale reference to removed doc: $path"
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      record_issue error "  ref: $line"
    done <<<"$refs"
  fi
done

deep_links="$(rg_scan_existing -n '\.\./\.\./\.\./docs/' --audit-paths crates/*/docs/README.md)"
if [[ -n "$deep_links" ]]; then
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    if [[ "$line" == *"codex-gemini-cli-parity-contract-v1.md"* ]]; then
      continue
    fi
    record_issue error "unexpected deep crate-docs cross-link: $line"
  done <<<"$deep_links"
fi

hash_cmd=""
if command -v shasum >/dev/null 2>&1; then
  hash_cmd="shasum"
elif command -v sha1sum >/dev/null 2>&1; then
  hash_cmd="sha1sum"
fi

# Like `rg_scan`, hashing distinguishes a valid empty result from an execution
# failure. Surface failures via record_issue rather than swallowing them.
# nullglob (scoped to the subshell) keeps an unmatched `crates/*/docs` from
# reaching find as a literal path.
if [[ -z "$hash_cmd" ]]; then
  record_issue error "missing required hash command: install shasum or sha1sum"
elif ! md_hashes="$(
  shopt -s nullglob
  find docs crates/*/docs -type f -name '*.md' -print0 \
    | xargs -0 "$hash_cmd" \
    | awk '{print $1}'
)"; then
  record_issue error "failed to enumerate or hash markdown payloads with $hash_cmd"
else
  dup_hashes="$(printf '%s\n' "$md_hashes" | sort | uniq -d)"
  if [[ -n "$dup_hashes" ]]; then
    while IFS= read -r hash; do
      [[ -n "$hash" ]] || continue
      record_issue error "duplicate markdown payload hash detected: $hash"
    done <<<"$dup_hashes"
  fi
fi

# Legacy-removal guardrails (reintroduction detection)
legacy_docs_hits="$(rg_scan_existing -n --hidden --glob '!.git' -S '\blegacy\b' --audit-paths \
  docs/specs docs/runbooks BINARY_DEPENDENCIES.md crates/*/README.md crates/*/docs)"
if [[ -n "$legacy_docs_hits" ]]; then
  record_issue error "legacy keyword reintroduced in active docs"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  doc-hit: $line"
  done <<<"$legacy_docs_hits"
fi

legacy_rs_hits="$(rg_scan_existing -n --hidden --glob '!.git' --glob '*.rs' -S '\blegacy\b' \
  --audit-paths crates)"
if [[ -n "$legacy_rs_hits" ]]; then
  record_issue error "legacy keyword reintroduced in Rust sources"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  rs-hit: $line"
  done <<<"$legacy_rs_hits"
fi

removed_redirect_hits="$(rg_scan_existing -n -S 'handle_legacy_redirect|"provider" \| "debug" \| "workflow" \| "automation"' \
  --audit-paths crates/codex-cli/src/main.rs crates/gemini-cli/src/main.rs)"
if [[ -n "$removed_redirect_hits" ]]; then
  record_issue error "removed codex/gemini redirect surfaces were reintroduced"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  redirect-hit: $line"
  done <<<"$removed_redirect_hits"
fi

removed_alias_hits="$(rg_scan_existing -n -S 'window-name|visible_alias = "enter"|Backward-compatible aliases are still accepted' \
  --audit-paths crates/macos-agent/src/cli.rs crates/macos-agent/README.md)"
if [[ -n "$removed_alias_hits" ]]; then
  record_issue error "removed macos-agent alias surfaces were reintroduced"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  alias-hit: $line"
  done <<<"$removed_alias_hits"
fi

removed_websocket_hits="$(rg_scan_existing -n -S 'top-level send|receiveTimeoutSeconds|or top-level send' \
  --audit-paths crates/api-testing-core/src/websocket/schema.rs crates/api-websocket/docs/specs/websocket-request-schema-v1.md)"
if [[ -n "$removed_websocket_hits" ]]; then
  record_issue error "removed websocket fallback surfaces were reintroduced"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  websocket-hit: $line"
  done <<<"$removed_websocket_hits"
fi

removed_image_ops_hits="$(rg_scan_existing -n -S 'Operation::(AutoOrient|Resize|Rotate|Crop|Pad|Flip|Flop|Optimize)|legacy transform|Legacy transform' \
  --audit-paths crates/image-processing/src crates/image-processing/README.md BINARY_DEPENDENCIES.md)"
if [[ -n "$removed_image_ops_hits" ]]; then
  record_issue error "removed image-processing transform surfaces were reintroduced"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  image-hit: $line"
  done <<<"$removed_image_ops_hits"
fi

for warn in "${warnings[@]}"; do
  echo "WARN: $warn"
done

if [[ ${#errors[@]} -gt 0 ]]; then
  for err in "${errors[@]}"; do
    echo "FAIL: $err"
  done
  echo "FAIL: docs hygiene audit (strict=$strict, errors=${#errors[@]}, warnings=${#warnings[@]})"
  exit 1
fi

echo "PASS: docs hygiene audit (strict=$strict, warnings=${#warnings[@]})"
