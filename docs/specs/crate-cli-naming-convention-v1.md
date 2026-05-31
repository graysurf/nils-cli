# Crate / CLI Naming Convention v1

## Purpose

Defines the mandatory naming convention for crates and their binaries in the
`nils-cli` workspace, and the CI audit that enforces it. Companion to
`docs/runbooks/new-cli-crate-development-standard.md`, which already requires
`nils-<name>` package names; this spec makes the full convention explicit and
enforced.

## Scope

Applies to every crate under `crates/`. Enforced by
`scripts/ci/crate-naming-audit.sh`, wired into the workspace check stack
(`scripts/ci/nils-cli-checks-entrypoint.sh`).

## Rules

### Crate directory

- `crates/<dir>` is lowercase kebab-case.
- A user/service-facing CLI tool directory ends in `-cli`
  (for example `forge-cli`, `git-cli`, `memo-cli`).
- An internal shared library crate is prefixed `nils-`
  (for example `nils-common`, `nils-term`, `nils-markdown`).
- A domain/service crate uses a plain descriptive name
  (for example `agent-docs`, `screen-record`, `plan-archive`).

### Package name

- `[package].name` MUST be `nils-<dir>`.
- Exception: when `<dir>` already starts with `nils-`, the package name is
  `<dir>` (no `nils-nils-` double prefix).

### Binary name

- Each `[[bin]].name` MUST equal the crate directory `<dir>`.
- A crate MAY omit `[[bin]]` when it is library-only.

### Documented exceptions (grandfathered)

These crates predate this convention and are published to crates.io; renaming
them would be a breaking change, so they are allowlisted in the audit. New
crates MUST NOT rely on these patterns.

| Crate dir | Package name | Binary name(s) | Reason |
| --- | --- | --- | --- |
| `plan-issue` | `nils-plan-issue` | `plan-issue-local` | Second `plan-issue-local` binary diverges from `<bin> == <dir>` |
| `nils-markdown` | `nils-markdown` | `md-render` | Published binary |
| `agent-workflow-primitives` | `nils-agent-workflow-primitives` | multi-tool set (`agent-run`, `browser-session`, ...) | One crate, many primitive binaries |

## Enforcement

`scripts/ci/crate-naming-audit.sh` validates every crate under `crates/` and
fails on any non-allowlisted deviation. Adding a new exception requires
updating BOTH the audit allowlist and the table above, each with a one-line
justification.
