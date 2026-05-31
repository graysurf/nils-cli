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
    refs="$(rg -n --fixed-strings "$path" "${existing_reference_roots[@]}" \
      -g '!**/docs/plans/**' \
      -g '!docs/specs/workspace-doc-retention-matrix-v1.md' \
      -g '!**/tests/**' \
      -g '!**/target/**' || true)"
  fi
  if [[ -n "$refs" ]]; then
    record_issue error "stale reference to removed doc: $path"
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      record_issue error "  ref: $line"
    done <<<"$refs"
  fi
done

deep_links="$(rg -n '\.\./\.\./\.\./docs/' crates/*/docs/README.md || true)"
if [[ -n "$deep_links" ]]; then
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    if [[ "$line" == *"codex-gemini-cli-parity-contract-v1.md"* ]]; then
      continue
    fi
    record_issue error "unexpected deep crate-docs cross-link: $line"
  done <<<"$deep_links"
fi

dup_hashes="$(find docs crates/*/docs -type f -name '*.md' -print0 \
  | xargs -0 shasum \
  | awk '{print $1}' \
  | sort \
  | uniq -d || true)"
if [[ -n "$dup_hashes" ]]; then
  while IFS= read -r hash; do
    [[ -n "$hash" ]] || continue
    record_issue error "duplicate markdown payload hash detected: $hash"
  done <<<"$dup_hashes"
fi

# Legacy-removal guardrails (reintroduction detection)
legacy_docs_hits="$(rg -n --hidden --glob '!.git' -S '\blegacy\b' \
  docs/specs docs/runbooks BINARY_DEPENDENCIES.md crates/*/README.md crates/*/docs 2>/dev/null || true)"
if [[ -n "$legacy_docs_hits" ]]; then
  record_issue error "legacy keyword reintroduced in active docs"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  doc-hit: $line"
  done <<<"$legacy_docs_hits"
fi

legacy_rs_hits="$(rg -n --hidden --glob '!.git' --glob '*.rs' -S '\blegacy\b' crates || true)"
if [[ -n "$legacy_rs_hits" ]]; then
  record_issue error "legacy keyword reintroduced in Rust sources"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  rs-hit: $line"
  done <<<"$legacy_rs_hits"
fi

removed_redirect_hits="$(rg -n -S 'handle_legacy_redirect|"provider" \| "debug" \| "workflow" \| "automation"' \
  crates/codex-cli/src/main.rs crates/gemini-cli/src/main.rs 2>/dev/null || true)"
if [[ -n "$removed_redirect_hits" ]]; then
  record_issue error "removed codex/gemini redirect surfaces were reintroduced"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  redirect-hit: $line"
  done <<<"$removed_redirect_hits"
fi

removed_alias_hits="$(rg -n -S 'window-name|visible_alias = "enter"|Backward-compatible aliases are still accepted' \
  crates/macos-agent/src/cli.rs crates/macos-agent/README.md 2>/dev/null || true)"
if [[ -n "$removed_alias_hits" ]]; then
  record_issue error "removed macos-agent alias surfaces were reintroduced"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  alias-hit: $line"
  done <<<"$removed_alias_hits"
fi

removed_websocket_hits="$(rg -n -S 'top-level send|receiveTimeoutSeconds|or top-level send' \
  crates/api-testing-core/src/websocket/schema.rs crates/api-websocket/docs/specs/websocket-request-schema-v1.md 2>/dev/null || true)"
if [[ -n "$removed_websocket_hits" ]]; then
  record_issue error "removed websocket fallback surfaces were reintroduced"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    record_issue error "  websocket-hit: $line"
  done <<<"$removed_websocket_hits"
fi

removed_image_ops_hits="$(rg -n -S 'Operation::(AutoOrient|Resize|Rotate|Crop|Pad|Flip|Flop|Optimize)|legacy transform|Legacy transform' \
  crates/image-processing/src crates/image-processing/README.md BINARY_DEPENDENCIES.md 2>/dev/null || true)"
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
