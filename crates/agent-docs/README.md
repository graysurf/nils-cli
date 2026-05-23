# agent-docs

## Overview

`agent-docs` is a deterministic policy-document discovery CLI for Codex/agent workflows.

It resolves required Markdown documents by context and scope, with explicit precedence rules for:

- startup policy files (`AGENTS.override.md` > `AGENTS.md`)
- home vs project extension config (`AGENT_DOCS.toml`)
- strict vs non-strict missing-doc behavior

The CLI does not replace runtime `AGENTS.md` loading. It provides a testable resolution contract.

## Non-goals

- Replacing or bypassing how any runtime natively loads `AGENTS.md`.
- Auto-editing arbitrary existing policy files in-place.
- Discovering non-markdown policy files without explicit `AGENT_DOCS.toml` entries.
- Remote policy sync or network-backed policy lookups.

## Path Resolution Precedence

Path resolution is deterministic and applies to all commands.

### `AGENT_DOCS_HOME`

1. `--docs-home <path>` (command flag)
2. `AGENT_DOCS_HOME` environment variable

If neither is provided, the binary errors with
`AGENT_DOCS_HOME is required; set AGENT_DOCS_HOME or pass --docs-home`.

> The previous `--agent-home` flag and `AGENT_HOME` env var were retired in
> this release; no fallback is shipped. Adapters that previously exported
> `AGENT_HOME` to drive `agent-docs` must rename it to `AGENT_DOCS_HOME` (or
> pass `--docs-home`) when upgrading.

### `PROJECT_PATH`

1. `--project-path <path>` (command flag)
2. `PROJECT_PATH` environment variable
3. `git rev-parse --show-toplevel` from current working directory
4. current working directory

### Normalization Rules

- Paths are normalized lexically (`.` removed, duplicate separators collapsed).
- Relative document paths in `AGENT_DOCS.toml` are resolved from their declared `scope` root.
- Absolute document paths are kept absolute and ignore scope root joining.

## Scope and Context Model

### Scopes

- `home`: rooted at effective `AGENT_DOCS_HOME`
- `project`: rooted at effective `PROJECT_PATH`

### Built-in contexts

- `startup`
- `skill-dev`
- `task-tools`
- `project-dev`

### Built-in required docs by context

| Context       | Scope     | Required document contract                              | Required |
| ------------- | --------- | ------------------------------------------------------- | -------- |
| `startup`     | `home`    | Use `AGENTS.override.md` when present; else `AGENTS.md` | `true`   |
| `startup`     | `project` | Use `AGENTS.override.md` when present; else `AGENTS.md` | `true`   |
| `skill-dev`   | `home`    | `DEVELOPMENT.md`                                        | `true`   |
| `task-tools`  | `home`    | `core/policies/cli-tools.md`                            | `true`   |
| `project-dev` | `project` | `DEVELOPMENT.md`                                        | `true`   |

`AGENTS.override.md` precedence is evaluated per scope independently.

## Command Surface

```text
Usage:
  agent-docs [OPTIONS] <command> [options]

Commands:
  resolve            Resolve required docs for a context
  contexts           List supported contexts
  add                Upsert one AGENT_DOCS.toml entry
  scaffold-agents    Scaffold default AGENTS.md template
  baseline           Check baseline doc coverage
  scaffold-baseline  Scaffold missing baseline docs
  completion         Print shell completion script (bash, zsh)
```

Use `agent-docs --help` (or `agent-docs <command> --help`) for CLI help text.

### Top-level options

These options sit before the subcommand and apply to every command:

- `--docs-home <path>` — override `AGENT_DOCS_HOME` root path
- `--project-path <path>` — override project root path
- `--worktree-fallback <auto|local-only>` (default: `auto`) — `auto` enables
  linked-worktree fallback to the primary worktree; `local-only` disables fallback
  and enforces local project files only
- `-h`, `--help` — print help
- `-V`, `--version` — print version

For convenience, the same `--docs-home`, `--project-path`, and
`--worktree-fallback` flags are also accepted positionally on each subcommand.

## Commands and Flags

### `contexts`

Print supported context names.

Flags:

- `--format text|json` (default: `text`)
- `--docs-home <path>`
- `--project-path <path>`

### `resolve`

Resolve effective required/optional docs for one context.

Flags:

- `--context startup|skill-dev|task-tools|project-dev` (required)
- `--format text|json|checklist` (default: `text`)
- `--strict` (missing required docs become exit code `1`)
- `--docs-home <path>`
- `--project-path <path>`

Format guidance:

- `text`: human-readable output for manual inspection and debugging.
- `json`: machine-readable output for structured parsing/integration.
- `checklist`: line-oriented required-doc contract for shell verification and CI guards.

### `add`

Create/update one `AGENT_DOCS.toml` entry in home or project scope.

Flags:

- `--target home|project` (required)
- `--context startup|skill-dev|task-tools|project-dev` (required)
- `--scope home|project` (required; target root for `path` resolution)
- `--path <doc-path>` (required)
- `--required` (set `required=true`; omitted means `required=false`)
- `--when <condition>` (default: `always`)
- `--notes <text>`
- `--docs-home <path>`
- `--project-path <path>`

### Copy-pastable `resolve` + `add` flow

```bash
# 1) Resolve built-in project-dev requirements
agent-docs resolve --context project-dev --format text

# 2) Register BINARY_DEPENDENCIES.md as required for project-dev
agent-docs add \
  --target project \
  --context project-dev \
  --scope project \
  --path BINARY_DEPENDENCIES.md \
  --required \
  --when always \
  --notes "External runtime tools required by the repo"
```

`add` stdout shape:

```text
add: target=project action=<inserted|updated> config=<PROJECT_PATH>/AGENT_DOCS.toml entries=<N>
```

Verify both built-in and extension docs are present:

```bash
agent-docs resolve --context project-dev --format checklist \
  | rg "REQUIRED_DOCS_BEGIN|REQUIRED_DOCS_END|DEVELOPMENT\\.md|BINARY_DEPENDENCIES\\.md"
```

### `scaffold-agents`

Scaffold default `AGENTS.md` template.

Flags:

- `--target home|project` (required)
- `--output <path>` (optional explicit output file path)
- `--force` (overwrite when file exists)
- `--docs-home <path>`
- `--project-path <path>`

Semantics:

- Default output: `<target-root>/AGENTS.md`
- Without `--force`, existing target file is not modified.

### `baseline`

Audit minimum baseline documents.

Flags:

- `--check` (run baseline check mode; required to produce a baseline report)
- `--target home|project|all` (default: `all`)
- `--format text|json` (default: `text`)
- `--strict` (missing required baseline docs become exit code `1`)
- `--docs-home <path>`
- `--project-path <path>`

### `scaffold-baseline`

Scaffold baseline docs from deterministic templates/inputs.

Flags:

- `--target home|project|all` (default: `all`)
- `--missing-only` (create only missing baseline docs)
- `--force` (overwrite existing files)
- `--dry-run` (print planned writes only)
- `--format text|json` (default: `text`)
- `--docs-home <path>`
- `--project-path <path>`

### `completion`

Print a shell completion script to stdout. Pipe the output into your shell's
completion loader.

Usage:

```bash
agent-docs completion <bash|zsh>
```

Arguments:

- `<SHELL>` — one of `bash`, `zsh`

## Exit Codes

- `0`: success (or non-strict missing-doc report)
- `1`: strict policy failure (`--strict` and one or more required docs missing)
- `2`: usage error (invalid flags, invalid command, missing required argument)
- `3`: config/schema error (`AGENT_DOCS.toml` invalid)
- `4`: runtime error (I/O failure, git probe failure not recoverable)

## Strict Semantics

### `resolve --strict`

- Required docs are evaluated after built-ins + TOML merge.
- If any required doc is missing on disk, command exits `1`.
- Without `--strict`, missing required docs are reported but exit code remains `0`.

### `baseline --strict`

- Evaluates baseline required docs for selected target scope(s).
- Missing required baseline docs cause exit `1` only when `--strict` is set.

## Worktree fallback

Worktree fallback is deterministic and applies only to project-scope required docs when running from a linked worktree in `auto` mode.

### Fallback order (project scope)

`startup` project policy:

1. `<PROJECT_PATH>/AGENTS.override.md`
2. `<PROJECT_PATH>/AGENTS.md`
3. `<PRIMARY_WORKTREE_PATH>/AGENTS.override.md` (fallback)
4. `<PRIMARY_WORKTREE_PATH>/AGENTS.md` (fallback)

`project-dev` required project docs (built-ins and required project-scope extension entries):

1. `<PROJECT_PATH>/<doc-path>`
2. `<PRIMARY_WORKTREE_PATH>/<doc-path>` (fallback)

### Strict and compatibility semantics

- `--strict` exits `1` only when all candidates in the deterministic order are missing.
- `local-only` mode disables `<PRIMARY_WORKTREE_PATH>` fallback candidates and enforces local project paths only.
- Non-worktree repositories are unchanged: only `<PROJECT_PATH>` candidates are evaluated.

### Output disclosure and local-only operation

When fallback is used, output must disclose fallback provenance in addition to required-doc presence.

- `text`/`json`: include a fallback source marker and the resolved fallback path.
- `checklist`: keep required-doc status lines and include fallback provenance in the same report.

To disable fallback, run resolve/baseline in `local-only` mode.

```bash
agent-docs --worktree-fallback local-only resolve --context startup --strict --format checklist
agent-docs --worktree-fallback local-only baseline --check --target project --strict --format text
```

## Output Contract

### `resolve` text example

```text
$ agent-docs resolve --context startup
CONTEXT: startup
AGENT_DOCS_HOME: /Users/example/.agents
PROJECT_PATH: /Users/example/work/nils-cli

[required] startup home /Users/example/.agents/AGENTS.override.md source=builtin status=present why="startup home policy (AGENTS.override.md preferred over AGENTS.md)"
[required] startup project /Users/example/work/nils-cli/AGENTS.md source=builtin-fallback status=present why="startup project policy (AGENTS.override.md missing, fallback AGENTS.md)"

summary: required_total=2 present_required=2 missing_required=0 strict=false
```

### `resolve` JSON example

```json
{
  "context": "startup",
  "strict": false,
  "docs_home": "/Users/example/.agents",
  "project_path": "/Users/example/work/nils-cli",
  "is_linked_worktree": false,
  "git_common_dir": null,
  "primary_worktree_path": null,
  "documents": [
    {
      "context": "startup",
      "scope": "home",
      "path": "/Users/example/.agents/AGENTS.override.md",
      "required": true,
      "status": "present",
      "source": "builtin",
      "why": "startup home policy (AGENTS.override.md preferred over AGENTS.md)"
    },
    {
      "context": "startup",
      "scope": "project",
      "path": "/Users/example/work/nils-cli/AGENTS.md",
      "required": true,
      "status": "present",
      "source": "builtin-fallback",
      "why": "startup project policy (AGENTS.override.md missing, fallback AGENTS.md)"
    }
  ],
  "summary": {
    "required_total": 2,
    "present_required": 2,
    "missing_required": 0
  }
}
```

The `is_linked_worktree`, `git_common_dir`, and `primary_worktree_path` fields are
emitted on every `resolve --format json` invocation. They are populated only when
the project root is a linked Git worktree (see "Worktree fallback").

### `resolve` checklist example

Checklist mode is designed for copy-paste verification. The required-doc section is delimited by `REQUIRED_DOCS_BEGIN` and
`REQUIRED_DOCS_END`, with one required document per line: `<filename> status=<present|missing> path=<absolute-path>`.

```text
$ agent-docs resolve --context project-dev --format checklist
REQUIRED_DOCS_BEGIN context=project-dev mode=non-strict
DEVELOPMENT.md status=present path=/Users/example/work/nils-cli/DEVELOPMENT.md
BINARY_DEPENDENCIES.md status=present path=/Users/example/work/nils-cli/BINARY_DEPENDENCIES.md
REQUIRED_DOCS_END required=2 present=2 missing=0 mode=non-strict context=project-dev
```

### `resolve` checklist strict + missing required example

When `--strict` is set and any required doc is missing, checklist output is still emitted and the process exits with code `1`.

```text
$ agent-docs resolve --context skill-dev --format checklist --strict
REQUIRED_DOCS_BEGIN context=skill-dev mode=strict
DEVELOPMENT.md status=missing path=/Users/example/.agents/DEVELOPMENT.md
REQUIRED_DOCS_END required=1 present=0 missing=1 mode=strict context=skill-dev
$ echo $?
1
```

### `baseline --check` text example

```text
$ agent-docs baseline --check --target all
BASELINE CHECK: all
AGENT_DOCS_HOME: $HOME/.agents
PROJECT_PATH: $HOME/work/nils-cli

[home] startup policy $HOME/.agents/AGENTS.md required present source=builtin-fallback why="startup home policy (AGENTS.override.md missing, fallback AGENTS.md)"
[home] skill-dev $HOME/.agents/DEVELOPMENT.md required missing source=builtin why="skill development guidance from AGENT_DOCS_HOME/DEVELOPMENT.md"
[home] task-tools $HOME/.agents/core/policies/cli-tools.md required present source=builtin why="tool-selection guidance from AGENT_DOCS_HOME/core/policies/cli-tools.md"
[project] startup policy $HOME/work/nils-cli/AGENTS.md required present source=builtin-fallback why="startup project policy (AGENTS.override.md missing, fallback AGENTS.md)"
[project] project-dev $HOME/work/nils-cli/DEVELOPMENT.md required present source=builtin why="project development guidance from PROJECT_PATH/DEVELOPMENT.md"

missing_required: 1
missing_optional: 0
suggested_actions:
  - agent-docs scaffold-baseline --missing-only --target home
```

### `baseline --check` JSON example

```json
{
  "target": "all",
  "strict": false,
  "docs_home": "/Users/example/.agents",
  "project_path": "/Users/example/work/nils-cli",
  "items": [
    {
      "scope": "home",
      "context": "skill-dev",
      "label": "skill-dev",
      "path": "/Users/example/.agents/DEVELOPMENT.md",
      "required": true,
      "status": "missing",
      "source": "builtin",
      "why": "skill development guidance from AGENT_DOCS_HOME/DEVELOPMENT.md"
    }
  ],
  "missing_required": 1,
  "missing_optional": 0,
  "suggested_actions": ["agent-docs scaffold-baseline --missing-only --target home"]
}
```

### `baseline` extension merge order (deterministic)

`baseline --check` applies extension documents with explicit, deterministic merge rules:

1. Start with built-in baseline items for selected `--target`.
2. Load extension configs in fixed order: `$AGENT_DOCS_HOME/AGENT_DOCS.toml` then `$PROJECT_PATH/AGENT_DOCS.toml`.
3. Consider only extension entries with `required = true` and `scope` included by `--target`.
4. Resolve each extension path, then de-dup by key: `(context, scope, normalized_path)`.
5. Same-key override order:
   - within one config file, later `[[document]]` wins (last-write-wins)
   - across files, project config wins over home config (loaded later)
6. Output order is stable:
   - built-ins stay in built-in declaration order
   - extension items keep first-seen key order; when overridden, the item is replaced in place

This ensures baseline output is reproducible while still honoring later overrides.

## `AGENT_DOCS.toml` Schema

Each scope may define `AGENT_DOCS.toml` at:

- `$AGENT_DOCS_HOME/AGENT_DOCS.toml`
- `$PROJECT_PATH/AGENT_DOCS.toml`

Schema uses repeated `[[document]]` tables.

```toml
[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"
notes = "Track required external CLIs for this project"
```

### Field contract

| Field      | Type   | Required | Rules                                                       |
| ---------- | ------ | -------- | ----------------------------------------------------------- |
| `context`  | string | yes      | One of: `startup`, `skill-dev`, `task-tools`, `project-dev` |
| `scope`    | string | yes      | One of: `home`, `project`                                   |
| `path`     | string | yes      | Relative or absolute path to markdown doc                   |
| `required` | bool   | no       | Default `false`                                             |
| `when`     | string | no       | Default `always`; supported values: `always`                |
| `notes`    | string | no       | Free text, default empty string                             |

### Deterministic merge contract

For `resolve --context <ctx>`:

1. Start with built-in entries for `<ctx>`.
2. Load entries from `$AGENT_DOCS_HOME/AGENT_DOCS.toml` (if file exists).
3. Load entries from `$PROJECT_PATH/AGENT_DOCS.toml` (if file exists).
4. Filter entries by exact `context == <ctx>`.
5. Normalize each entry path:
   - absolute path: keep as-is
   - relative path: join with root selected by entry `scope`
6. De-dup with merge key: `(context, scope, normalized_path)`.
7. Conflict resolution order:
   - built-in keys are immutable and cannot be removed or downgraded.
   - for non-built-in duplicates, later source wins (`project` config overrides `home` config).
   - within one file, later table wins (last-write-wins).
8. Final output order is stable:
   - built-ins in built-in declaration order
   - merged extension entries in load order after de-dup replacement

This merge behavior is deterministic and test-friendly.

### De-dup examples

- Same key appears twice in `$PROJECT_PATH/AGENT_DOCS.toml`: second entry wins.
- Same key appears in both home and project configs: project entry wins.
- Key matches built-in required doc (for example project `DEVELOPMENT.md` in `project-dev`): built-in contract remains required and present
  in output.

## Invalid Schema Behavior

If any `AGENT_DOCS.toml` entry is invalid, command exits `3` and prints actionable error.

### Error example: invalid context

```text
error[AGENT_DOCS_SCHEMA]: /Users/example/work/nils-cli/AGENT_DOCS.toml:4:11
invalid value for `context`: "project"
allowed: startup, skill-dev, task-tools, project-dev
```

### Error example: missing required field

```text
error[AGENT_DOCS_SCHEMA]: /Users/example/.agents/AGENT_DOCS.toml:1:1
missing required key `path` in [[document]]
```

### Error example: unsupported `when`

```text
error[AGENT_DOCS_SCHEMA]: /Users/example/work/nils-cli/AGENT_DOCS.toml:7:8
invalid value for `when`: "if-env:CI"
allowed: always
```

## Explicit `BINARY_DEPENDENCIES.md` Support Example

Add required `BINARY_DEPENDENCIES.md` for `project-dev` context.

CLI command:

```bash
agent-docs add \
  --target project \
  --context project-dev \
  --scope project \
  --path BINARY_DEPENDENCIES.md \
  --required \
  --when always \
  --notes "External runtime tools required by the repo"
```

Equivalent TOML entry:

```toml
[[document]]
context = "project-dev"
scope = "project"
path = "BINARY_DEPENDENCIES.md"
required = true
when = "always"
notes = "External runtime tools required by the repo"
```

`resolve --context project-dev` must include this document after merge, without removing built-in project `DEVELOPMENT.md`.

Verification command:

```bash
agent-docs resolve --context project-dev --format checklist \
  | rg "REQUIRED_DOCS_BEGIN|REQUIRED_DOCS_END|DEVELOPMENT\\.md|BINARY_DEPENDENCIES\\.md"
```

## Snapshot Fixture Maintenance

The `add` command has golden/snapshot fixtures under `tests/fixtures/add`.

- Run snapshot-related tests:

  ```bash
  scripts/ci/agent-docs-snapshots.sh
  ```

- Re-generate expected snapshots (`--bless`) and immediately verify:

  ```bash
  scripts/ci/agent-docs-snapshots.sh --bless
  ```

## Docs

- [Docs index](docs/README.md)
