# nils-common

`nils-common` is the workspace shared helper crate for cross-CLI primitives.

Primary constraint: shared helpers must preserve behavioral parity for each consuming CLI. Moving logic into this crate must not change
user-facing output text, warnings, color behavior, or exit-code contracts.

## Shared helper policy

Workspace-wide shared-crate boundary and extraction-lane decisions are documented in
[`docs/specs/workspace-shared-crate-boundary-v1.md`](../../docs/specs/workspace-shared-crate-boundary-v1.md).
Workspace-level keep/delete ownership decisions are tracked in
[`docs/specs/workspace-doc-retention-matrix-v1.md`](../../docs/specs/workspace-doc-retention-matrix-v1.md).

### What belongs in `nils-common`

- Reusable helper logic used by multiple CLI crates.
- Domain-neutral primitives (process/env/shell/git/clipboard/fs internals).
- APIs that return structured results and let callers own final UX text.
- Behavior that can be covered by deterministic unit tests.

### What stays crate-local

- User-facing warning/error text (including emoji/prefix wording).
- Exit-code mapping and command-level failure policy.
- CLI-specific command composition and UX defaults.
- Product/business/domain flows that only make sense in one crate.
- GitHub adapters and `gh` command orchestration (for example issue/PR write workflows).

## Modules and purpose

- `env`: truthy parsing helpers, env-presence checks, `NO_COLOR` and prompt-segment color toggles, duration parsing, and trimmed non-empty
  env lookup.
- `shell`: POSIX single-quote escaping (with selectable escape style) and ANSI stripping modes.
- `process`: command execution wrappers (`run_output`/`run_checked`/`run_stdout_trimmed`/`run_status_*`), PATH lookup helpers, and headless
  browser-launch detection.
- `git`: `git` command wrappers for repo probes, `rev-parse` helpers, staged-path listing, name-status-z parsing, lockfile detection, and
  scope suggestion primitives for commit tooling.
- `clipboard`: best-effort clipboard copy with explicit tool priority.
- `fs`: atomic write, timestamp write/remove, UTF-8 text write, SHA-256 hashing, and cross-platform replace helpers with structured errors.
- `markdown`: markdown payload validation (with violation reporting), markdown-table-safe cell canonicalization, markdown heading/code-block
  rendering, and stable JSON pretty-format helpers used by orchestration/reporting CLIs.
- `provider_runtime`: provider-runtime substrate (paths, profiles, auth persistence, exec invocation, JSON/JWT helpers, structured errors)
  shared by Codex/Gemini-style CLIs without provider-specific UX copy.
- `rate_limits_ansi`: shared rate-limit table cell formatting (current-profile coloring and percent-band coloring) honoring `NO_COLOR`.

The crate also exposes a tiny top-level `greeting(name: &str) -> String` helper used by `cli-template` for the new-crate smoke test.

## API examples

`env`:

```rust
use nils_common::env;

let prompt_segment_enabled = env::env_truthy_or("AGENTS_CLI_PROMPT_SEGMENT", false);
let no_color = env::no_color_enabled();
let maybe_agent_home = env::env_non_empty("AGENT_HOME");
println!("prompt_segment={prompt_segment_enabled}, no_color={no_color}");
```

`shell`:

```rust
use nils_common::shell::{self, AnsiStripMode, SingleQuoteEscapeStyle};

let quoted = shell::quote_posix_single_with_style("a'b", SingleQuoteEscapeStyle::Backslash);
let plain = shell::strip_ansi("\x1b[31mred\x1b[0m", AnsiStripMode::CsiSgrOnly);
assert_eq!(quoted, "'a'\\''b'");
assert_eq!(plain, "red");
```

`process`:

```rust
use nils_common::process;

assert!(process::cmd_exists("git"));
let git_path = process::find_in_path("git").expect("git should be on PATH");
let out = process::run_stdout_trimmed(git_path.to_string_lossy().as_ref(), &["--version"])
    .expect("git --version should run");
println!("{out}");
```

`git`:

```rust
use nils_common::git;

let inside = git::is_inside_work_tree().expect("git check should run");
if inside {
    let root = git::repo_root().expect("repo root check");
    let staged = git::staged_name_only().expect("staged list");
    let scope = git::suggested_scope_from_staged_paths(&staged);
    println!("repo root: {root:?}");
    println!("suggested scope: {scope}");
}
```

`clipboard`:

```rust
use nils_common::clipboard::{copy_best_effort, ClipboardOutcome, ClipboardPolicy, ClipboardTool};

let tool_order = [
    ClipboardTool::Pbcopy,
    ClipboardTool::WlCopy,
    ClipboardTool::Xclip,
    ClipboardTool::Xsel,
    ClipboardTool::Clip,
];
let outcome = copy_best_effort("hello", &ClipboardPolicy::new(&tool_order));

if matches!(
    outcome,
    ClipboardOutcome::SkippedNoTool | ClipboardOutcome::SkippedFailure
) {
    eprintln!("clipboard copy unavailable; keep crate-local fallback messaging");
}
```

`fs`:

```rust
use nils_common::fs::{self, AtomicWriteError, SECRET_FILE_MODE};
use std::path::Path;

fs::write_atomic(Path::new("cache/auth.json"), br#"{"ok":true}"#, SECRET_FILE_MODE)?;
fs::write_timestamp(
    Path::new("cache/auth.json.timestamp"),
    Some("2026-02-01T00:00:00Z\n"),
)?;
let digest = fs::sha256_file(Path::new("cache/auth.json"))?;

if let Err(AtomicWriteError::CreateParentDir { path, .. }) =
    fs::write_atomic(Path::new("/tmp/demo.json"), b"{}", SECRET_FILE_MODE)
{
    eprintln!("parent directory error: {path:?}");
}

println!("sha256={digest}");
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Migration conventions for parity

When introducing a shared helper at a call site:

1. Add or keep characterization tests in the caller crate first.
2. Move only primitive logic; keep a crate-local adapter for message formatting and exit-code mapping. For `write_atomic` /
   `write_timestamp` / `sha256_file` migrations, map structured errors back to existing crate-local UX text.
3. Preserve existing quote/ANSI mode choices and `NO_COLOR` behavior.
4. Keep tool/command fallback order identical (for example clipboard tool order, git probe fallback behavior).
5. Re-run crate tests that cover the touched command paths before merging.

## Non-goals

These mirror the workspace shared-crate-boundary spec
([`docs/specs/workspace-shared-crate-boundary-v1.md`](../../docs/specs/workspace-shared-crate-boundary-v1.md)) at the crate level:

- Moving provider-specific message wording, JSON envelope copy, or exit-code mapping into `nils-common`.
- Merging Codex and Gemini command-level UX into one behavior surface (parity-sensitive secret-dir routing stays crate-local until
  characterization proves a safe merge).
- Treating `nils-term` as a generic runtime helper crate; progress bars, spinners, and TTY presentation policy stay in `nils-term`.
- Defining CLI-specific UX copy, warning templates, or emoji policy.
- Owning command-level business logic for a single CLI.
- Owning shared GitHub operation adapters such as `nils-common::github` (keep crate-local adapters).
- Hiding meaningful behavior differences that should remain explicit in local adapters.
- Keeping compatibility-only wrappers once shared helpers are canonical.
- Replacing specialized shared crates such as `api-testing-core`, `nils-term`, or `nils-test-support`.

## Docs

- [Docs index](docs/README.md)
- [markdown helpers contract v1](docs/specs/markdown-helpers-contract-v1.md)
