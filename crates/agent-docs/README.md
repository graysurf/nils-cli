# agent-docs

## Overview

`agent-docs` resolves and audits the documents and validation contract selected
for a repository. A repository normally owns that policy in `AGENT_DOCS.toml`;
an exact user-local rule can instead select a private project catalog stored
outside every target Git worktree. The binary remains a generic resolver and
auditor with no hardcoded required documents.

It is built for two non-agent-facing jobs:

- `audit` — repo health: install-symlink wiring, declared-doc presence and
  content validity, and catalog validity, for CI and a daily healthcheck.
- `preflight --intent X` — resolve what THIS repo requires for an intent (the
  document set and the per-repo validation contract), emitted in a versioned
  JSON shape that consuming hooks inject and enforce.

Plus durable selective intent state (`session`), repository catalog management
(`init` / `explain` / `list` / `remove`), user-local rule management (`config`),
and typed automatic-integration decisions (`integration resolve`).

The agent does not run a per-task `agent-docs` preflight: always-on policy is
delivered by the harness (auto-loaded prompt files), intent docs are
hook-injected, and enforcement happens at the finish line. See the cross-repo
design in `sympoies/agent-runtime-kit`
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
| `config enroll/exclude/show/list/remove` | Preview or apply exact user-local enrollment and exclusion rules. |
| `integration resolve` | Resolve a typed integration action and content-bound fingerprint. |
| `session activate/prepare/context/status/verify` | Persist and verify intent activation; `context` returns a bounded DSH policy decision. |
| `completion` | Generate shell completion scripts. |

There is no top-level `resolve` command. The retired `baseline` / `scaffold-*` /
`add` / `contexts` commands and `startup` per-task context remain absent.

### Global options

- `--docs-home <PATH>` — override the docs-home root.
- `--project-path <PATH>` — override the project root.
- `--worktree-fallback <auto|local-only>` — linked-worktree fallback mode.
- `--user-config` — opt a catalog-consuming operation into the effective
  user-local integration decision; `preflight`, `explain`, `list`, and `session`
  require an accompanying `--product`.
- `--integration-fingerprint <SHA256>` — with `--user-config`, require the
  current decision to equal a previously resolved fingerprint.

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

By default, two catalog files are loaded and merged: the **docs-home** catalog
(`<docs_home>/AGENT_DOCS.toml`, the shared defaults a repo inherits) and the
**repository project** catalog (`<project>/AGENT_DOCS.toml`, per-repo
overrides). When the docs-home and project are the same directory the file is
loaded once.

An effective user enrollment replaces only the repository project catalog with
one external private project catalog. The private catalog retains project-scope
semantics: relative document paths, predicates, and validation commands remain
rooted at the target project. To preserve the stable preflight v2 contract,
resolved private documents report `source = "project"`; physical provenance is
reported separately as `selected_catalog.origin = "user"` by
`integration resolve`. Private and repository project catalogs are never merged;
if both are selected, `integration resolve` returns `block/catalog-conflict`.
The docs-home catalog still combines with whichever project catalog is selected.

A catalog declares two array-of-tables sections:

```toml
# A required (or conditionally required) document.
[[document]]
context  = "project-dev"            # free-form intent identifier
scope    = "project"                # home | project | global
path     = "DEVELOPMENT.md"         # relative to the scope root
product  = "codex"                  # optional: codex | claude | hermes | dsh | a list
phase    = ["edit", "review"]       # optional: free-form phase name or a list
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
single catalog tag or a non-empty list. Supported tags are `codex`, `claude`,
`hermes`, and the isolated runtime tag `dsh`. Unscoped entries apply to every
view; stable product commands select their matching tag, while DSH-only
commands select unscoped plus DSH-tagged entries.

### Phases

`phase` is optional on `[[document]]` (only — `[[validation]]` is intent-level
and is not phase-scoped). It accepts a single phase string or a non-empty list of
phase strings, mirroring `product`. Phase names are **free-form** (the same
charset as a context: ASCII alphanumerics plus `-_./`); the binary hard-codes no
phase vocabulary, so a consumer defines its own phases (for example `edit`,
`review`, `delivery`) and adding a phase never needs a release.

A document with **no** `phase` field applies to **every** phase (symmetric with
"no `product` = all products"). `preflight --intent X --phase P` resolves the
no-phase documents plus the documents whose declared phases include `P`, and
excludes documents scoped to a different phase. Omitting `--phase` applies no
phase filter and returns every document (today's behavior, byte-for-byte). A
`--phase` value that matches only no-phase documents is **valid** — it returns
the no-phase set rather than erroring — so a phase with no dedicated documents
still works. A malformed `--phase` value is a usage error (exit `64`).

`--phase` is accepted by `preflight`, `session prepare`, `session context`, and
`session verify` (one phase per call, "the current phase"). The provider-only
`session prerequisite` command requires the exact `edit` phase. `--intent`
stays repeatable except for `session context` and `session prerequisite`, which
require exactly one intent.

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

## Private project configuration

The fixed user registry is `$XDG_CONFIG_HOME/agent-docs/config.toml`, falling
back to `$HOME/.config/agent-docs/config.toml` when `XDG_CONFIG_HOME` is unset.
The selected root must be absolute. The strict schema is:

```toml
schema_version = 1

[[project]]
match = "project-path" # or "git-common-dir"
path = "/canonical/absolute/path"
mode = "enroll" # or "exclude"
catalog = "/absolute/private/catalog.toml" # enroll only
reason = "optional local explanation"
```

Rules use exact canonical filesystem identity. `project-path` matches one
worktree; `git-common-dir`, created with `--all-worktrees`, matches all
worktrees of one local clone. Remote URLs, provider slugs, globs, prefixes, and
regular expressions are not identities. Multiple effective matches block as
ambiguous.

Mutating commands are previews unless `--apply` is explicit:

```bash
agent-docs config enroll --catalog /abs/private/project.toml --reason "local policy"
agent-docs config enroll --catalog /abs/private/project.toml --apply
agent-docs config exclude --all-worktrees --apply
agent-docs config show --format json
agent-docs config list --format json
agent-docs config remove --all-worktrees --apply
```

`enroll`, `exclude`, and `remove` modify only the user registry. They never
write, stage, ignore, move, or delete files in the target repository. Updates
retain unrelated rules and comments, take a descriptor-held advisory lock,
re-read while locked, and atomically replace and sync the registry. The lock
file remains in place and a process crash releases the descriptor lock without
leaving a stale create-only sentinel. On Unix, CLI-created configuration
directories use mode `0700`; config and lock files use `0600`.

The registry and selected private catalog are limited to one MiB and must be
current-user-owned regular files, with neither the final path nor an ancestor
resolved through a symlink, and must not be group/world writable. Rule reasons
are limited to 500 bytes. A private catalog must be absolute, canonical, and
outside the target Git common directory and every target worktree. Private
catalog document paths must be relative and remain inside the target worktree;
selected document reads reject symlinks and bind validation and emitted content
to one opened file snapshot. These private-file restrictions do not change
repository-owned `AGENT_DOCS.toml` permissions.

Resolve policy before enabling a consumer:

```bash
agent-docs integration resolve --product claude --format json
agent-docs --user-config --integration-fingerprint "$FINGERPRINT" \
  preflight --intent project-dev --product claude --format json
```

`integration resolve` returns one of four typed actions:

- `integrate`: `user-enrollment` or `repository-catalog` selected one catalog;
- `exclude`: an exact `user-exclusion` matched;
- `unmanaged`: `no-catalog` selected nothing;
- `block`: for example `catalog-conflict`, `ambiguous-user-rule`, or an invalid
  or unavailable selected catalog.

Malformed, unreadable, or insecure global user configuration never grants an
enrollment or exclusion. A valid repository catalog remains integrated with a
diagnostic; otherwise the project remains unmanaged with a diagnostic. By
contrast, a matched enrollment whose selected private catalog is unsafe,
missing, or invalid blocks integration.

The decision fingerprint binds the canonical project and common-dir identity,
matched selector/mode/reason, typed action and reason code, selected catalog
origin/path/content digest, selected docs-home catalog digest, product,
fallback mode, and schema version. Unrelated registry entries are excluded, so
their edits do not stale a binding. Pass the fingerprint back with
`--user-config --integration-fingerprint` to prevent time-of-check/time-of-use
drift.

### Migration and rollback

To migrate repository-local private policy, first copy the catalog outside all
target worktrees, secure it, preview and then apply `config enroll`, resolve and
record a product-specific fingerprint, and only then opt consumers into
`--user-config`. Do not delete or move the repository catalog through config
management; enrollment intentionally blocks while a repository catalog is
present, so repository changes remain a separate reviewed operation.

To roll back consumer behavior immediately, stop passing `--user-config` and
`--integration-fingerprint`; default commands continue to use only repository
policy. To remove the stored rule, run the matching
`config remove [--all-worktrees] --apply`. `config exclude` is an explicit
automatic-integration policy, not a rollback of enrollment.

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
- `phase` is **omitted** when no `--phase` filter was supplied (so a no-phase
  call is byte-identical to the pre-phase shape); with `--phase P` it is the
  requested phase string.
- `documents[].phases` is **omitted** for a document that declares no phase
  (applies to all phases); otherwise it lists the document's declared phase
  names. Because these additions are omitted when absent, catalogs that use no
  phases produce identical output and identical `session` fingerprints.
- `documents[].content` is the full document body, emitted so a hook can inject
  the doc without re-reading the file. It is present only for resolved, present
  documents (omitted for missing ones).
- `documents[].source` describes the stable catalog layer: `home` or `project`.
  A private selected catalog is the effective project layer and therefore emits
  `project`; use `integration resolve`'s `selected_catalog.origin` to distinguish
  repository and user physical provenance.
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

Phase-aware resolution adds `resolve_intent_with_catalog_for_scope`, which takes
an optional `Product` and an optional `Phase`; the `_for_product` entrypoint
delegates to it with `phase = None`, so existing callers are unaffected. Catalog
entries carry `phases`, resolved documents report the catalog phases that
matched, and preflight reports include the selected `phase`.

The product dimension is also part of the public model types used by this crate:
catalog entries carry `products`, resolved documents report the catalog
products that matched, and preflight reports include the selected `product`.
Rust callers that construct those structs directly should update their literals
for the v2 product model; CLI JSON consumers should treat
`agent-docs.preflight.v2` and `agent-docs.audit.v2` as the product-aware
contract boundaries.

The public Rust `Product` enum remains the stable closed
`Codex | Claude | Hermes` contract. `dsh` is parsed as an isolated catalog tag,
not exposed as a fourth public enum variant. Generic product-aware commands
project DSH-only entries out (while retaining known tags from mixed arrays);
the DSH-specific context, integration, and validation helpers project the same
fully validated catalog to unscoped plus DSH-tagged entries. This keeps
exhaustive downstream Rust matches source-compatible within 1.x.

Rust integrations use the additive `agent_docs::dsh` boundary rather than a
`Product::Dsh` variant. `validation_contracts_from_roots` returns the fully
validated unscoped-plus-DSH validation view. `session_intent_is_current` is a
read-only, fail-closed check for the exact activation written by `session
context`; it never creates or refreshes session state. The DSH pre-edit policy
gate uses that narrow verifier instead of widening generic `session verify`.

Private catalog provenance is intentionally internal to the resolver and the
new integration-decision response. It does not add a public `DocumentSource`
variant or a required field to public `ScopeCatalog` literals, and it does not
expand the closed `documents[].source` value domain of preflight v2.

## Selective intent session state

Session activation performs strict declared-intent preflight and writes an
atomic `agent-docs.session.v2` state record keyed by hashes of the session id
and canonical repository root. Stored records contain no raw session id or
machine path. Activation records the effective integration fingerprint;
verification re-resolves that decision plus document and catalog data, so a
relevant policy or selected-content change reports
`stale-integration-decision`. Unrelated registry edits preserve validity.
`session status --integration-fingerprint <value>` compares the requested
fingerprint directly with the stored activation. Prior record schemas remain
unsupported for status and verify; an explicit activate or prepare first
validates the current catalog and then replaces a v1 record rather than
silently carrying its intents forward. JSON responses expose `record_file`
relative to `--state-home`; consumers that need the local file join the two
paths without persisting a machine-specific state-home value.

```bash
agent-docs --user-config session activate --session-id "$SESSION_ID" \
  --product codex --state-home "$STATE_HOME" --intent project-dev --format json
agent-docs --user-config session status --session-id "$SESSION_ID" \
  --product codex --state-home "$STATE_HOME" --format json
agent-docs --user-config --integration-fingerprint "$FINGERPRINT" \
  session verify --session-id "$SESSION_ID" --product codex \
  --state-home "$STATE_HOME" --require-intent project-dev --format json
```

Omit `--user-config` for the repository-catalog-only behavior. Products without
hooks may still use these shared CLI records, but the record does not claim that
product-native hooks invoked activation.

### Bounded DSH context decision

`session context` is the single-call policy loading boundary for DeepSeek
Harness. It accepts the normal session scope, exactly one `--intent`, an
optional caller-supplied `--phase`, and a caller-generated `--request-id`. It is
valid only for `--product dsh`. DSH is intentionally not accepted by generic
`list`, `preflight`, `session prepare`, `session status`, or `session verify`
product selectors. While holding the same per-record lock, `context`
strictly resolves the selected catalog, refreshes or creates the activation,
and returns the current satisfied required documents. Repeating the call still
returns those documents after compaction; only `decision.reason` changes from
`prepared` to `already-current` when the fingerprint was already current.

```bash
agent-docs session context --session-id "$SESSION_ID" --product dsh \
  --state-home "$STATE_HOME" --intent project-dev --phase edit \
  --request-id "$REQUEST_ID" --max-bytes 20480 --format json
```

The success envelope uses `cli.agent-docs.session.context.v1` and its only data
field is `decision`. That object uses `decision.context.v1` and contains the
echoed request id, product, intent, optional phase, reason, `verified = true`,
ordered `documents`, `document_count`, and `total_bytes`. Each document contains
only `source`, `scope`, and `content`; the response never includes the catalog,
optional documents, validation commands, document paths, record path, raw
session id, or project path. This decision supplies context only and does not
authorize a tool execution. The optional phase merely records the exact
caller-supplied resolution scope; it is not decision authority. Repeating the
same `session context` call is the verification/replay surface for DSH policy.

Request ids are 1–128 ASCII bytes: an alphanumeric first character followed by
alphanumerics or `-_.:`. The content budget defaults to 20 KiB and has a 64 KiB
hard cap; at most 128 currently required documents may be returned.
`total_bytes` is the sum of returned UTF-8 document content bytes. Optional and
condition-false documents are not opened. Every emitted document must use a
relative traversal-free catalog path, resolve inside its declared scope (or a
verified linked-worktree fallback), and be a regular non-symlink file. If the
complete required set exceeds either budget, the command fails with
`context-budget-exceeded`; it emits no partial policy and writes no activation
for that request. An unsafe document fails with `context-document-unsafe` under
the same atomic rule. Omitting `--phase` intentionally retains the existing
full prepare semantics and includes required documents from every phase;
callers that know the current workflow phase should pass it to select the
no-phase plus matching phase documents. A failed context refresh never mutates
the prior record; the caller repairs the catalog or increases the requested
budget within the hard cap, then repeats `session context`.

### Transactional DSH prerequisites

`session prerequisite` is the side-effect-free counterpart used by a DSH
runtime before policy admission. It requires `--phase edit` and accepts the
same DSH intent, request, and response-budget fields as `session context`, plus
exact bounded agent, workspace-generation, call, turn/step, tool, and
visible-definition identifiers. It uses the verifier's default catalog
selection: `--user-config`, `--integration-fingerprint`, and
`--worktree-fallback local-only` are rejected. It returns
`decision.prerequisite.v1`: both `reason = pending` and `already-current`
include a bounded opaque receipt so every admitted execution can be freshly
revalidated after approval and guard waits. Neither result writes or refreshes
a session activation. A full activation still covers ordinary phase
verification, but the first phase prerequisite conservatively returns
`pending` and materializes a phase activation so its returned content and reuse
decision come from one resolved fingerprint.

After DSH waterfall, approval, monotonic guards, cancellation, and definition
revalidation admit the call, the runtime invokes `session commit-prerequisite`
with the same execution binding and receipt. Commit re-resolves the catalog
under the session-record lock and writes only when the receipt fingerprint is
still current. A changed policy returns `prerequisite-stale`; another session,
state home, repository, agent, workspace generation, call, turn/step, tool, or
definition returns `prerequisite-receipt-mismatch`. Abandoned receipts require
no cleanup because begin creates no pending state. Repeating an exact commit is
idempotent and returns `already-current`.

These commands are a machine-to-machine provider protocol. The receipt grants
no authority and contains only hashes plus public intent/phase identifiers;
the policy hook independently revalidates it. Human and explicit agent flows
may continue to use `session context`.

### Phase-scoped preparation

`session prepare` / `session verify` accept an optional `--phase <PHASE>`. A
`prepare --intent X --phase P` resolves and fingerprints the P-scoped document
set and records a phase-qualified activation; a `verify --require-intent X
--phase P` passes when the record holds a matching phase-scoped preparation
**or** a matching full (no-phase) preparation, since a full prepare covers every
phase's subset. Omitting `--phase` behaves exactly as before, and the no-phase
record shape is unchanged — the phase qualifier and the `data.phase` field are
stored/emitted only when a phase is supplied. A phase-scoped prepare whose
required documents are unsatisfied fails with the `phase-unsatisfied` error code
(the phase parallel of `preflight-unsatisfied`); a malformed `--phase` value
fails with `invalid-phase`.

```bash
agent-docs session prepare --session-id "$SESSION_ID" --product codex \
  --state-home "$STATE_HOME" --intent project-dev --phase edit --format json
agent-docs session verify --session-id "$SESSION_ID" --product codex \
  --state-home "$STATE_HOME" --require-intent project-dev --phase edit --format json
```

### Session failure recovery contract

Automation consumes `agent-docs session ... --format json`. Session failure
envelopes retain the existing `cli.agent-docs.session.<command>.v1` schemas and
add typed recovery metadata under `error.details`:

- `retryable` is a boolean;
- `next_action` is one stable value from the closed vocabulary below;
- `recovery` is a bounded object with a typed command or action, reusable input
  field names, and only the safe parameters needed for the next step.

The closed next-action vocabulary is:

- `fix-arguments`
- `list-declared-intents`
- `inspect-preflight`
- `prepare-intent`
- `refresh-integration-decision`
- `repair-catalog`
- `retry-bounded`
- `inspect-session-state`
- `upgrade-agent-docs`
- `report-invariant`

For example, missing or stale intent activation points to `session.prepare`
and identifies declared intents and phase when known. A stale integration
decision points first to `integration.resolve`, then to `session.prepare`.
Version 1 records are replaceable through prepare; future schemas require an
`agent-docs` upgrade and are never overwritten. Unrecognized schemas, other
corruption, and non-timeout lock or I/O failures require state inspection.
Lock timeouts allow only a bounded retry.

Recovery arrays and diagnostics are bounded. Failures never expose expanded
argv, raw session IDs, absolute state-home/project paths, secrets, environment
dumps, private catalog or document content, or raw command output. Text mode is
a single-line rendering of the same typed failure model. Automation MUST use
JSON `code` and `details`; it MUST NOT parse `message` or `hint`.

### Required preflight undeclared-intent contract

The separate `preflight --require-declared-intent` guard retains its existing
failure contract below; the session-specific privacy and recovery rules above
apply to `session` errors introduced or touched by this implementation.

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
| 3 | catalog/config error, including a bound decision that selects no catalog |
| 4 | runtime or internal invariant error |
| 64 | command-line usage error |
| 65 | stale bound data, or undeclared intent when required |

`--strict` makes `audit` and `preflight` exit `1` when the resolved state is not
clean (problems, or unsatisfied required documents). Typed integration policy
outcomes, including `exclude`, `unmanaged`, and `block`, are successful resolver
responses (exit `0`). A catalog-consuming command that binds `--user-config`
classifies failures consistently: no selected catalog is config exit `3`, a
stale `--integration-fingerprint` is data exit `65`, and resolution or invariant
failure is runtime exit `4`. `config` and `integration` honor `--format json` for
both success and failure envelopes, while malformed command lines use
`cli.agent-docs.error.v1` and exit `64`.

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
