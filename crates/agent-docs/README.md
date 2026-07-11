# agent-docs

## Overview

`agent-docs` resolves and audits the documents and validation contract a
repository declares in its `AGENT_DOCS.toml` catalog. Policy is **data the repo
owns**; the binary is a generic resolver and auditor with no hardcoded required
documents.

It is built for two non-agent-facing jobs:

- `audit` — repo health: install-symlink wiring, declared-doc presence and
  content validity, and catalog validity, for CI and a daily healthcheck.
- `preflight --intent X` — resolve what THIS repo requires for an intent (the
  document set and the per-repo validation contract), emitted in a versioned
  JSON shape that consuming hooks inject and enforce.

Plus durable selective intent state (`session`) and catalog management:
`init` / `explain` / `list` / `remove`.

The agent does not run a per-task `agent-docs` preflight: always-on policy is
delivered by the harness (auto-loaded prompt files), intent docs are
hook-injected, and enforcement happens at the finish line. See the cross-repo
design in `graysurf/agent-runtime-kit`
(`docs/plans/2026-05-30-agent-docs-redesign/`).

## Command surface

| Command | Purpose |
| --- | --- |
| `audit` | Repo health: wiring + declared-doc validity + catalog validity. |
| `preflight --intent X` | Resolve the doc set + validation contract for an intent (JSON for hooks). |
| `init` | Emit an annotated project-local override stub. |
| `explain` | Explain what an intent resolves to and why. |
| `list` | List declared documents, validation contracts, and intents. |
| `remove` | Remove a `[[document]]` entry from the project catalog. |
| `session activate/status/verify` | Persist and verify intent activation scoped to a session, repository, and product. |
| `completion` | Generate shell completion scripts. |

There are no `resolve` / `baseline` / `scaffold-*` / `add` / `contexts`
commands and no `startup` per-task context — they were retired in the engine
redesign.

### Global options

- `--docs-home <PATH>` — override the docs-home root.
- `--project-path <PATH>` — override the project root.
- `--worktree-fallback <auto|local-only>` — linked-worktree fallback mode.

## docs-home resolution

When `--docs-home` is omitted, the docs-home is resolved in this order:

1. `--docs-home <PATH>` flag.
2. The install symlink: the directory that `~/.claude/CLAUDE.md` (or the Codex
   equivalent `~/.codex/AGENTS.md`) resolves to, i.e.
   `dirname(readlink ~/.claude/CLAUDE.md)`.
3. The `AGENT_DOCS_HOME` environment variable (lowest-precedence fallback).
4. Otherwise a clear error instructing the caller to pass `--docs-home`.

`audit` reports the symlink wiring (intact / mismatch / missing) so a broken
install surfaces instead of failing silently.

## Catalog model

Two catalog files are loaded and merged: the **docs-home** catalog
(`<docs_home>/AGENT_DOCS.toml`, the shared defaults a repo inherits) and the
**project** catalog (`<project>/AGENT_DOCS.toml`, per-repo overrides). When the
docs-home and project are the same directory the file is loaded once.

A catalog declares two array-of-tables sections:

```toml
# A required (or conditionally required) document.
[[document]]
context  = "project-dev"            # free-form intent identifier
scope    = "project"                # home | project | global
path     = "DEVELOPMENT.md"         # relative to the scope root
product  = "codex"                  # optional: codex | claude | hermes | a list
required = true                     # default: false
when     = "path-exists:Cargo.toml" # default: always (see grammar below)
marker   = "## Validation"          # optional: content must contain this string
last-reviewed-within-days = 180     # optional freshness window
notes    = "why this document matters"

# A per-intent validation contract.
[[validation]]
context     = "project-dev"
commands    = ["bash scripts/ci/all.sh"]   # run before declaring done
product     = ["codex", "claude"]          # optional product filter
marker      = "target/.agent-validation-ok" # optional finish-line marker
description = "Run the full check stack before delivery."

# Optional repository-owned path classification shared by pre-edit and
# docs-impact gates. Overlap is reported as ambiguous; unmatched paths use
# the explicit fail-closed `unknown` class.
[path_classes]
production = ["src/**"]
test = ["tests/**"]
docs = ["docs/**", "**/*.md"]
generated = ["build/**"]
unmatched = "unknown"
```

### Scopes

- `home` / `global` — resolved against the docs-home root. `global` is allowed
  only in the docs-home catalog (it is a cross-repo pointer that applies to all
  projects).
- `project` — resolved against the project root.

### Contexts (intents)

Contexts are free-form identifiers (ASCII alphanumerics plus `-_./`) declared
by the catalog; they are not compiled in. `preflight --intent X` resolves every
document and validation entry whose `context` equals `X`.

### Products

`product` is optional on both `[[document]]` and `[[validation]]`. It accepts a
single product string or a non-empty list of product strings. Supported values
are `codex`, `claude`, and `hermes`. Unscoped entries apply to every product; scoped
entries are included only when the requested `--product` matches.

### `when` grammar

`when` is an OR of AND-clauses of `path-exists` atoms:

```text
when   := clause ("||" clause)*
clause := atom ("&&" atom)*
atom   := "path-exists:" <glob> | "always"
```

`&&` binds tighter than `||`. A `path-exists:<glob>` atom is true when at least
one filesystem path matching `<glob>` exists under the resolved project root.
Globs support `*`, `?`, `[...]`, and `**` (which matches across directory
segments). A document whose `when` evaluates false is not required — a docs-only
repo (no `Cargo.toml` / `package.json` / `src/**`) auto-skips code docs with no
manual opt-out.

### Content validation

A required document is satisfied only when it exists AND passes content
validation: non-empty, contains its declared `marker` (when one is declared),
and — if `last-reviewed-within-days` is set — carries a recent enough
`last-reviewed: YYYY-MM-DD` line. A scaffolded placeholder therefore fails.

Resolved documents are de-duplicated by resolved path; a project-catalog entry
overrides a docs-home entry that resolves to the same path.

## `preflight --intent X` JSON contract

`agent-docs preflight --intent <X> --format json` emits the
`agent-docs.preflight.v2` shape. This is the **cross-repo contract** consumed by
agent-runtime-kit hooks (start-of-task awareness injection and the finish-line
validation gate). The fields below are stable within the `v2` schema:

```json
{
  "schema_version": "agent-docs.preflight.v2",
  "intent": "project-dev",
  "product": "codex",
  "strict": false,
  "docs_home": "/abs/docs-home",
  "project_path": "/abs/project",
  "is_linked_worktree": false,
  "documents": [
    {
      "context": "project-dev",
      "scope": "project",
      "path": "/abs/project/DEVELOPMENT.md",
      "products": ["codex"],
      "declared_required": true,
      "required": true,
      "when": "path-exists:Cargo.toml || path-exists:package.json",
      "when_satisfied": true,
      "status": "present",
      "validation": {
        "exists": true,
        "non_empty": true,
        "marker_present": true,
        "freshness": "not-declared",
        "valid": true
      },
      "source": "home",
      "why": "home catalog /abs/docs-home/AGENT_DOCS.toml document, scope=project when=\"...\" (matched)",
      "content": "# Dev\n\n## Validation\n\nrun the tests\n"
    }
  ],
  "validation": {
    "context": "project-dev",
    "declared": true,
    "commands": ["bash scripts/ci/all.sh"],
    "description": "Run before declaring done."
  },
  "summary": {
    "required_total": 1,
    "satisfied_required": 1,
    "missing_required": 0,
    "invalid_required": 0
  }
}
```

Field notes:

- `product` is `null` when no `--product` filter was supplied. With
  `--product codex|claude`, documents and validation contracts with a matching
  catalog `product` field are included; unscoped entries are always included.
- `documents[].products` is empty for include-all entries, or lists the scoped
  product names from the catalog.
- `documents[].content` is the full document body, emitted so a hook can inject
  the doc without re-reading the file. It is present only for resolved, present
  documents (omitted for missing ones).
- `documents[].required` is `declared_required && when_satisfied`.
- `validation.declared` is `false` when no `[[validation]]` entry matches the
  intent (then `commands` is empty).
- `validation.marker` / `validation.description` are omitted when not declared.
- `summary.satisfied_required` counts required docs that are present and
  content-valid; `missing_required` and `invalid_required` break down the rest.

`preflight --require-declared-intent` is an opt-in guard for callers that
explicitly expect the intent to exist. Without the flag, an unknown intent keeps
the compatibility behavior above: exit `0`, `documents=[]`, and
`validation.declared=false`. With the flag, `preflight` exits `65` when the
requested intent is not declared by any applicable document entry or validation
contract before product filtering. Optional documents, product-filtered
documents, and documents skipped by `when` still count as a declared intent; so
do validation-only intents.

## Rust API note

Product-aware resolution is exposed through new resolver entrypoints such as
`resolve_intent_for_product`, `resolve_intent_with_catalog_for_product`,
`resolve_all_documents_for_product`, and
`all_validation_contracts_for_product`. The pre-existing resolver functions
remain available and behave as unfiltered calls.

The product dimension is also part of the public model types used by this crate:
catalog entries carry `products`, resolved documents report the catalog
products that matched, and preflight reports include the selected `product`.
Rust callers that construct those structs directly should update their literals
for the v2 product model; CLI JSON consumers should treat
`agent-docs.preflight.v2` and `agent-docs.audit.v2` as the product-aware
contract boundaries.

## Selective intent session state

Session activation performs strict declared-intent preflight and writes an
atomic state record keyed by hashes of the session id and canonical repository
root. Stored records contain no raw session id or machine path. Verification
re-resolves documents and catalog data, so changed content or configuration
invalidates stale activation.

```bash
agent-docs session activate --session-id "$SESSION_ID" --product codex \
  --state-home "$STATE_HOME" --intent project-dev --format json
agent-docs session status --session-id "$SESSION_ID" --product codex \
  --state-home "$STATE_HOME" --format json
agent-docs session verify --session-id "$SESSION_ID" --product codex \
  --state-home "$STATE_HOME" --require-intent project-dev --format json
```

Products without hooks may still use these shared CLI records, but the record
does not claim that product-native hooks invoked activation.

In text mode, the guarded failure is written to stderr:

```text
error: undeclared intent `no-such-intent`; available intents: project-dev, task-tools
```

In JSON mode, the guarded failure uses the shared CLI error envelope:

```json
{
  "schema_version": "cli.agent-docs.preflight.v2",
  "ok": false,
  "error": {
    "code": "undeclared-intent",
    "message": "intent `no-such-intent` is not declared for this project",
    "details": {
      "intent": "no-such-intent",
      "available_intents": ["project-dev", "task-tools"]
    }
  }
}
```

A consuming hook typically injects an awareness cue listing
`validation.commands` on `project-dev` / `task-tools` intent at the start of a
task, and at the finish line blocks turn-end when `validation.declared` is true,
non-doc code was edited, and there is no evidence the commands ran.

## `audit` JSON

`agent-docs audit --format json` emits `agent-docs.audit.v2`:

```json
{
  "schema_version": "agent-docs.audit.v2",
  "target": "all",
  "strict": false,
  "docs_home": "/abs/docs-home",
  "project_path": "/abs/project",
  "wiring": [
    { "name": "install-symlink", "ok": true, "detail": "~/.claude/CLAUDE.md -> /abs/docs-home" }
  ],
  "documents": [],
  "problems": 0,
  "suggested_actions": []
}
```

`problems` counts unsatisfied required documents plus failed wiring checks.
`suggested_actions` lists fix hints (it never repairs anything).

## `init`

`agent-docs init --print` writes an annotated `AGENT_DOCS.toml` override stub to
stdout. The stub lists the inherited docs-home defaults as comments, embeds the
schema and `when` grammar, and pre-fills `when` examples for a detected
`Cargo.toml` / `package.json`. It declares **no** active entries, so a project
that runs it and makes no edits adds zero requirements. Use `--dry-run` to
preview the target path without writing, or `--force` to write the file.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success |
| 1 | strict failure (unsatisfied required docs / audit problems) |
| 3 | catalog (config) error |
| 4 | runtime error |
| 64 | command-line usage error |
| 65 | undeclared intent when `preflight --require-declared-intent` is set |

`--strict` makes `audit` and `preflight` exit `1` when the resolved state is not
clean (problems, or unsatisfied required documents).

## Worktree fallback

For `scope = "project"` documents, when the project path is a linked git
worktree and `--worktree-fallback auto` (the default) is in effect, a document
missing in the linked worktree is resolved from the primary worktree. Pass
`--worktree-fallback local-only` to disable this and enforce local files only.

## Catalog validation errors

Invalid catalogs fail with a precise error naming the section, entry index, and
field, for example:

```text
/abs/AGENT_DOCS.toml [validation] document[0].when: unsupported atom `file-exists:x`; expected `path-exists:<glob>` or `always`
```
