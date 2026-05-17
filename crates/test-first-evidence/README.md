# test-first-evidence

## Overview

`test-first-evidence` records deterministic test-first evidence for agent workflows. It is designed for skills that need a stable record of
before-fix failing evidence, an explicit waiver when failing evidence is not practical, and final validation after implementation.

The command writes one JSON record under the artifact directory:

```text
DIR/test-first-evidence.json
```

## Package vs binary name

| Field        | Value                      |
| ------------ | -------------------------- |
| Package name | `nils-test-first-evidence` |
| Binary name  | `test-first-evidence`      |

## Usage

```text
Record test-first evidence and waivers for agent workflows.

Usage: test-first-evidence <COMMAND>

Commands:
  init            Create a deterministic evidence record
  record-failing  Record a failing test or reproducible failure before the fix
  record-waiver   Record an explicit waiver when failing evidence is not practical
  record-final    Record final validation after the implementation
  verify          Verify the evidence record is complete enough for delivery
  show            Print the current evidence record
  completion      Print shell completion script

Options:
  -h, --help     Print help
  -V, --version  Print version
```

Examples:

```bash
test-first-evidence init \
  --out "$AGENT_HOME/out/projects/acme__app/test-first" \
  --classification behavior-change \
  --production-path src/lib.rs

test-first-evidence record-failing \
  --out "$AGENT_HOME/out/projects/acme__app/test-first" \
  --command "cargo test bug_repro" \
  --exit-code 101 \
  --summary "bug reproduced before fix"

test-first-evidence record-final \
  --out "$AGENT_HOME/out/projects/acme__app/test-first" \
  --command "cargo test bug_repro" \
  --status pass

test-first-evidence verify --out "$AGENT_HOME/out/projects/acme__app/test-first" --format json
test-first-evidence completion zsh
```

## Commands

- `init --out DIR --classification TEXT [--production-path PATH ...] [--note TEXT ...] [--force] [--format text|json]`: create the
  deterministic record.
- `record-failing --out DIR --command TEXT --exit-code CODE --summary TEXT [--test-name TEXT] [--artifact PATH ...]
  [--format text|json]`: record before-fix failure evidence.
- `record-waiver --out DIR --reason TEXT [--substitute-validation TEXT ...] [--format text|json]`: record a waiver for docs-only,
  generated-only, no-harness, or otherwise impractical failing-test cases.
- `record-final --out DIR --command TEXT --status pass|fail [--summary TEXT] [--artifact PATH ...] [--format text|json]`: record final
  validation.
- `verify --out DIR [--format text|json]`: return success only when before-fix evidence or waiver exists and final validation is `pass`.
- `show --out DIR [--format text|json]`: print the current record.
- `completion <bash|zsh>`: print clap-generated shell completions.

## Output contract

Human-readable text is the default. JSON is opt-in with `--format json` on record commands.

JSON output uses versioned envelopes:

```json
{
  "schema_version": "cli.test-first-evidence.verify.v1",
  "command": "test-first-evidence verify",
  "ok": true,
  "result": {
    "record_file": "/tmp/evidence/test-first-evidence.json",
    "complete": true,
    "missing": [],
    "record": {
      "schema_version": "test-first-evidence.record.v1",
      "change_classification": "bug-fix"
    }
  }
}
```

Failure envelopes use `ok=false` with stable `error.code`, `error.message`, and optional `error.details`.

Exit codes:

- `0`: success
- `1`: runtime failure or incomplete evidence from `verify`
- `64`: usage/configuration error

## Secret-safety boundary

The command stores user-provided command lines and summaries, so it redacts common secret assignments and token-like values before writing
the record or printing JSON/text output. It does not read artifact file contents.

## Docs

- [Docs index](docs/README.md)
- [CLI service JSON contract guideline](../../docs/specs/cli-service-json-contract-guideline-v1.md)
- [New CLI crate development standard](../../docs/runbooks/new-cli-crate-development-standard.md)
