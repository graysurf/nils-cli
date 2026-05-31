#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill_root="$(cd "${script_dir}/.." && pwd)"
repo_root="$(cd "${skill_root}/../../.." && pwd)"

if [[ ! -f "${skill_root}/SKILL.md" ]]; then
  echo "error: missing SKILL.md" >&2
  exit 1
fi
if [[ ! -f "${skill_root}/scripts/create-cli-crate.sh" ]]; then
  echo "error: missing scripts/create-cli-crate.sh" >&2
  exit 1
fi

if [[ ! -f "${repo_root}/docs/runbooks/new-cli-crate-development-standard.md" ]]; then
  echo "error: missing runbook doc" >&2
  exit 1
fi
if [[ ! -f "${repo_root}/docs/specs/cli-service-json-contract-guideline-v1.md" ]]; then
  echo "error: missing JSON guideline doc" >&2
  exit 1
fi

bash "${skill_root}/scripts/create-cli-crate.sh" --help >/dev/null
bash "${skill_root}/scripts/create-cli-crate.sh" --project-path "${repo_root}" --mode plan >/dev/null
bash "${skill_root}/scripts/create-cli-crate.sh" --project-path "${repo_root}" --mode implement >/dev/null

echo "ok: project skill smoke checks passed"
