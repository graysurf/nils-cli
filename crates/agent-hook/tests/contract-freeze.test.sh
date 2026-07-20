#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
spec="$root/crates/agent-hook/docs/specs/agent-hook-v1.md"

grep -Fq '`SessionStart`' "$spec"
for capability in \
  decision.allow.v1 \
  decision.warn.v1 \
  decision.block.v1 \
  decision.context.v1 \
  decision.transform.v1 \
  agent-session.activity.v1 \
  agent-session.owner-liveness.v1 \
  agent-session.semantic-conflict.v1 \
  agent-session.coordination.v1 \
  runtime-kit.handler.v1; do
  grep -Fq "\`$capability\`" "$spec"
done
grep -Fq 'capability = { id = "runtime-kit.handler.v1", handler_id = "session-start-healthcheck" }' "$spec"
grep -Fq '`Write|Edit|NotebookEdit|MultiEdit|apply_patch`' "$spec"
grep -Fq 'The only expression operator is `|`.' "$spec"
grep -Fq 'Semantic conflict is never accepted from a Codex or Claude payload field.' "$spec"
grep -Fq 'Only a definite authenticated conflict blocks.' "$spec"
