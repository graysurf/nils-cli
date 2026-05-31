#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill_dir="$(cd "${script_dir}/.." && pwd)"
ref_dir="${skill_dir}/references"

usage() {
  cat <<'USAGE'
Usage:
  render-refactor-response-template.sh --mode <implement|no-action|both>

Modes:
  implement   Print implementation response template
  no-action   Print no-action response template
  both        Print implementation template, then no-action template
USAGE
}

mode=""

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --mode)
      if [[ $# -lt 2 ]]; then
        echo "error: --mode requires a value" >&2
        usage >&2
        exit 2
      fi
      mode="${2:-}"
      shift 2
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

if [[ -z "$mode" ]]; then
  echo "error: --mode is required" >&2
  usage >&2
  exit 2
fi

impl="${ref_dir}/IMPLEMENTATION_RESPONSE_TEMPLATE.md"
no_action="${ref_dir}/NO_ACTION_RESPONSE_TEMPLATE.md"

if [[ ! -f "$impl" ]]; then
  echo "error: missing template: $impl" >&2
  exit 1
fi
if [[ ! -f "$no_action" ]]; then
  echo "error: missing template: $no_action" >&2
  exit 1
fi

case "$mode" in
  implement)
    cat "$impl"
    ;;
  no-action)
    cat "$no_action"
    ;;
  both)
    cat "$impl"
    echo
    cat "$no_action"
    ;;
  *)
    echo "error: invalid --mode '${mode}' (expected implement|no-action|both)" >&2
    usage >&2
    exit 2
    ;;
esac
