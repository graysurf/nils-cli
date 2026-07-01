# Completion Migration Contract Template

## Purpose

This template defines the per-CLI completion migration contract for clap-first rollout work. Fill every required section so implementation,
completion enforcement, and validation stay deterministic across CLI migrations.

## Usage

1. Copy this file to the crate-local report path:
   - `crates/<crate>/docs/reports/<cli>-completion-migration-contract.md`
2. Replace placeholders with CLI-specific values.
3. Keep the contract updated through implementation and testing.
4. Link the contract path in PR notes before requesting review.

## Metadata

- CLI binary: `<cli>`
- Owning crate: `crates/<crate>`
- Contract owner: `<name>`
- Target PR: `<link-or-number>`
- Status: `draft|in-review|done`
- Last updated: `YYYY-MM-DD`
- Completion enforcement metadata tuple from matrix row:
  - `completion_mode=<clap-first>`
  - `completion_mode_toggles=<forbidden>`
  - `alternate_completion_dispatch=<forbidden>`
  - `generated_load_failure=<fail-closed>`
  - `completion_engine=<static|dynamic>` (optional; absent means `static`)

## command graph (required)

Document the clap command tree used as the completion source of truth.

| command path         | clap source              | completion obligations             | notes     |
| -------------------- | ------------------------ | ---------------------------------- | --------- |
| `<cli>`              | `crates/<crate>/src/...` | root flags + top-level subcommands | `<notes>` |
| `<cli> <subcommand>` | `crates/<crate>/src/...` | subcommand flags/args/candidates   | `<notes>` |

Checklist:

- [ ] Every supported subcommand path is listed.
- [ ] Long/short flags are represented by clap metadata.
- [ ] Hidden/deprecated paths are explicitly called out.

## value providers (required)

Document static and dynamic completion candidate sources.

| argument or flag | provider type (`ValueEnum`/`PossibleValue`/`ValueHint`/`CompleteEnv`/`__complete`) | source location          | context-aware behavior    | tests         |
| ---------------- | ---------------------------------------------------------------------------------- | ------------------------ | ------------------------- | ------------- |
| `<flag-or-arg>`  | `<provider>`                                                                       | `crates/<crate>/src/...` | `<cursor-position rules>` | `<test path>` |

Rules:

- Value providers stay clap-first; dynamic providers extend the generated baseline only.
- Runtime data lookups must describe deterministic fallback behavior.

Checklist:

- [ ] No global candidate dump behavior remains.
- [ ] Cursor-position filtering is documented for dynamic value paths.

## alias map (required)

Document alias coverage and canonical command rewrites.

| alias     | canonical command rewrite | zsh alias entry               | bash alias entry                | completion registration point       |
| --------- | ------------------------- | ----------------------------- | ------------------------------- | ----------------------------------- |
| `<alias>` | `<canonical command>`     | `completions/zsh/aliases.zsh` | `completions/bash/aliases.bash` | `completions/<shell>/<cli adapter>` |

Checklist:

- [ ] Alias entries are synced in both alias files, or `not required` is explicit.
- [ ] Adapter rewrite semantics are documented when aliases inject defaults.

## completion enforcement metadata (required)

Declare and validate metadata that enforces clap-first behavior and forbids runtime mode switches or alternate completion dispatch paths.
Values must match the CLI row in `docs/specs/completion-coverage-matrix-v1.md`.

| metadata key                    | required value | declared value      | enforcement location | verification method                                                            |
| ------------------------------- | -------------- | ------------------- | -------------------- | ------------------------------------------------------------------------------ |
| `completion_mode`               | `clap-first`   | `<value>`           | `<path>`             | `<proof>`                                                                      |
| `completion_mode_toggles`       | `forbidden`    | `<value>`           | `<path>`             | `rg -n "COMPLETION_MODE" <paths>`                                              |
| `alternate_completion_dispatch` | `forbidden`    | `<value>`           | `<path>`             | `rg -n "alternate completion\|fallback completer\|old completion" <paths>`     |
| `generated_load_failure`        | `fail-closed`  | `<value>`           | `<path>`             | `<completion test case>`                                                       |
| `completion_engine`             | `static`       | `<static\|dynamic>` | `<path>`             | matrix row engine value; `dynamic` requires a `CompleteEnv` registration asset |

Checklist:

- [ ] Declared metadata values match required values in this template.
- [ ] Declared metadata values match the matrix row for this CLI.
- [ ] Verification evidence includes completion-mode toggle and alternate dispatch checks.

## single-path invariants (required)

List invariants that enforce the metadata above and keep completion clap-first and fail closed.

| invariant                                                 | enforcement location | verification method                                                        |
| --------------------------------------------------------- | -------------------- | -------------------------------------------------------------------------- |
| No runtime completion-mode toggles                        | `<path>`             | `rg -n "COMPLETION_MODE" <paths>`                                          |
| No alternate completion dispatch path                     | `<path>`             | `rg -n "alternate completion\|fallback completer\|old completion" <paths>` |
| Generated-load failure fails closed (empty/no candidates) | `<path>`             | `<completion test case>`                                                   |

Checklist:

- [ ] Adapters are thin (registration + optional dynamic hooks only).
- [ ] Generated-load failure does not route to alternate completion code.

## tests and validation (required)

Record command-level and repository-level checks for this migration.

### validation commands

1. `zsh -f tests/zsh/completion.test.zsh`
2. `zsh -n completions/zsh/_<cli>`
3. `bash -n completions/bash/<cli>`
4. `<cli> completion zsh | rg -- "--help|--version|--"`
5.

   ```bash
   rg -n "COMPLETION_MODE|completion_mode_toggles|alternate_completion_dispatch|generated_load_failure" \
     docs/specs/completion-coverage-matrix-v1.md \
     crates/<crate>/docs/reports/<cli>-completion-migration-contract.md
   ```

1. `bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast`
   - use `--docs-only` only when all changed files are docs.
   - record GitHub required-check status before merge.

### test coverage mapping

| requirement                     | test file or command | status (`pending/pass/fail`) | notes     |
| ------------------------------- | -------------------- | ---------------------------- | --------- |
| command graph candidates        | `<path-or-command>`  | `<status>`                   | `<notes>` |
| value providers                 | `<path-or-command>`  | `<status>`                   | `<notes>` |
| alias map registration          | `<path-or-command>`  | `<status>`                   | `<notes>` |
| completion enforcement metadata | `<path-or-command>`  | `<status>`                   | `<notes>` |
| single-path invariants          | `<path-or-command>`  | `<status>`                   | `<notes>` |

## acceptance criteria (required)

Mark all items before closing the migration.

- [ ] command graph matches implemented clap command surface.
- [ ] value providers cover required candidates and dynamic paths.
- [ ] alias map reflects zsh/bash alias entries and completion registration.
- [ ] completion enforcement metadata is declared, matches matrix policy, and is validated.
- [ ] single-path invariants are enforced and verified.
- [ ] tests and validation commands pass, with evidence captured.
- [ ] PR notes link this contract and summarize follow-up risk (if any).
