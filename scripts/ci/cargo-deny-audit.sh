#!/usr/bin/env bash
# Run the cargo-deny security-advisory and duplicate-version checks.
#
# This mirrors the `cargo-deny` GitHub Actions job and is the local entry point
# for the same gate. Scope is intentionally limited to `advisories` (RUSTSEC)
# and `bans` (duplicate-version control); see `deny.toml` for the policy.
#
# Requires the `cargo-deny` binary (`cargo install cargo-deny --locked`, or
# `brew install cargo-deny`).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

if ! command -v cargo-deny >/dev/null 2>&1; then
  echo "error: cargo-deny is not installed (cargo install cargo-deny --locked)" >&2
  exit 127
fi

exec cargo deny check advisories bans "$@"
