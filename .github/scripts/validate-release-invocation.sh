#!/usr/bin/env bash
set -euo pipefail

event_name="${1:-}"
actor="${2:-}"
triggering_actor="${3:-}"
run_attempt="${4:-}"
release_ref="${5:-}"

is_broker_identity() {
  case "$1" in
    xsin4880|"dobi-bot[bot]") return 0 ;;
    *) return 1 ;;
  esac
}

[[ "$run_attempt" =~ ^[1-9][0-9]*$ ]] || {
  echo "error: release workflow run attempt is invalid" >&2
  exit 1
}

case "$event_name" in
  push)
    if ((run_attempt > 1)) && ! is_broker_identity "$triggering_actor"; then
      echo "error: release workflow reruns are restricted to trusted broker identities" >&2
      exit 1
    fi
    ;;
  workflow_dispatch)
    if ! is_broker_identity "$actor" || ! is_broker_identity "$triggering_actor"; then
      echo "error: workflow_dispatch release recovery is restricted to trusted broker identities" >&2
      exit 1
    fi
    ;;
  *)
    echo "error: unsupported release workflow event" >&2
    exit 1
    ;;
esac

if [[ ! "$release_ref" =~ ^refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: release workflow requires a stable v-prefixed tag ref, got $release_ref" >&2
  exit 1
fi
