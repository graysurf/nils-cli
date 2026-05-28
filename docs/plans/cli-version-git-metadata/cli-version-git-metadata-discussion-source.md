# CLI version git metadata — Source

| Field              | Value                                                                                                                                                               |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Status             | Ready for plan generation                                                                                                                                           |
| Date               | 2026-05-29                                                                                                                                                          |
| Source             | Discussion 2026-05-29: dev builds between release tags all report the same semver via `env!("CARGO_PKG_VERSION")`, so a built binary cannot be traced to its commit |
| Intended next step | Generate a plan to add a shared zero-dependency build-info crate and wire `--version` long output across binary crates                                              |

## Purpose

nils-cli is a ~35-crate workspace sharing one `[workspace.package]` version
(`0.26.1`). Every CLI renders its version directly from
`env!("CARGO_PKG_VERSION")`. Between two release tags the version is not
bumped, so every dev/local build reports the same string and cannot be traced
to the commit (or to a dirty working tree) it was built from. Embedding git
metadata at build time closes this gap without changing the release-version
contract.

## Confirmed facts

- Workspace shares `version = "0.26.1"` via `[workspace.package]` in the root
  `Cargo.toml`.
- Binary crates render version via `env!("CARGO_PKG_VERSION")` (e.g.
  `crates/cli-template/src/main.rs:141`, `crates/plan-tooling/src/cli.rs:73`,
  and multiple `completion.rs` `.version(...)` calls).
- No `build.rs` currently injects version/git metadata, and no
  `vergen`/`git2`/`gix` dependency exists in `Cargo.lock`. The existing
  `crates/screen-record/build.rs` is unrelated.
- The release flow (`nils-cli-bump-version-tag-release`) bumps the shared
  workspace version, tags, and updates the homebrew tap; semver remains the
  release source of truth.
- `crates/agent-runtime-cli/tests/integration/cli.rs:33` asserts `--version`
  output **contains** `env!("CARGO_PKG_VERSION")` (substring, not equality), so
  appending build metadata keeps this test green as long as the semver stays a
  substring.
- Adding a dependency triggers the `third-party-artifacts` (THIRD_PARTY_LICENSES
  regen) and `Cargo.lock` locked-build CI gates; a zero-dependency `build.rs`
  triggers neither.

## Decisions (locked at this source doc)

1. Keep semver in `[workspace.package]` as the release source of truth; git
   metadata is additive and exists only for traceability.
2. Embed `git describe --tags --always --dirty` (not a bare SHA) so a single
   token encodes the commit, distance from the last tag, and dirty state.
   Example: `0.26.1-14-gabc1234-dirty`.
3. Use a hand-rolled, zero-dependency `build.rs` rather than
   `vergen`/`gix`/`git2`, to avoid the license-audit and locked-build CI gates
   for a purely internal tool.
4. Host the build-info in a dedicated leaf crate `nils-build-info` (not
   `nils-common`). Rationale: a `build.rs` with `rerun-if-changed=.git/HEAD`
   recompiles its host crate whenever HEAD moves; isolating it in a small leaf
   crate keeps that rebuild cascade off the widely-depended `nils-common`. Only
   binary crates that render `--version` depend on `nils-build-info`.
5. clap layering: `-V` (short) keeps clean semver via
   `version = env!("CARGO_PKG_VERSION")`; `--version` (long) uses clap
   `long_version` to show `<semver> (<git-describe>, rustc <ver>)`. Both contain
   the semver substring, satisfying the existing integration test.
6. `build.rs` must degrade gracefully when `.git` is absent (crates.io tarball,
   homebrew bottle): fall back to a placeholder so the build never fails, and
   released builds resolve to clean semver (where `git describe` lands exactly
   on the tag).
7. Adopt: omit a wall-clock build timestamp from the version string to preserve
   reproducible builds; `git describe` is the sole traceability token. If a
   build date is later wanted, gate it on `SOURCE_DATE_EPOCH`.
8. Rollout (resolved 2026-05-29): wire every binary crate that renders
   `--version` in a single PR — no pilot-first phase.
9. Long-version content (resolved 2026-05-29): include the rustc version, and
   use the single-line format `<semver> (<git-describe>, rustc <ver>)`. cargo
   sets the `RUSTC` env var for build scripts, so the rustc version is captured
   in the same zero-dependency `build.rs`.

## Reference build.rs (zero dependency)

```rust
use std::process::Command;

fn main() {
    let describe = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NILS_GIT_DESCRIBE={describe}");

    // rustc version: cargo sets RUSTC for build scripts, so this stays
    // zero-dependency.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_ver = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NILS_RUSTC_VERSION={rustc_ver}");

    println!("cargo:rerun-if-changed=.git/HEAD");
    // Also rerun when the checked-out ref's tip moves.
    println!("cargo:rerun-if-changed=.git/refs");
}
```

The host crate then exposes consts/helpers, e.g.
`pub const GIT_DESCRIBE: &str = env!("NILS_GIT_DESCRIBE");`,
`pub const RUSTC_VERSION: &str = env!("NILS_RUSTC_VERSION");`, and a
`long_version()` that concatenates the semver, `GIT_DESCRIBE`, and the rustc
version into the single-line format above, baked in at compile time of
`nils-build-info`.

## Scope

- New leaf crate `nils-build-info` exposing build-metadata consts plus a
  `long_version()` helper.
- Wire binary crates' clap definitions to use `nils_build_info::long_version()`
  for `long_version`.

## Non-scope

- Replacing or changing the release / version-bump flow.
- Adding `vergen` or any git library dependency.
- A machine-readable `version --json` / `--build-info` subcommand (possible
  later).
- Changing `env!("CARGO_PKG_VERSION")` usages that are not user-facing
  `--version` output (e.g. the `web-evidence` user-agent string).

## Implementation boundaries

- Do not break `crates/agent-runtime-cli/tests/integration/cli.rs` — keep the
  semver as a substring of `--version`.
- Do not add a `build.rs` to `nils-common` or any widely-depended lib crate.
- No new third-party dependency (preserve clean `third-party-artifacts` and
  locked-build gates).

## Requirements

- `<bin> --version` (long) includes semver, `git describe --tags --always
  --dirty` output, and the rustc version.
- `<bin> -V` (short) prints clean semver.
- Build succeeds with no `.git` present, emitting a deterministic fallback.
- HEAD changes are reflected on rebuild (rerun-if-changed wired).

## Acceptance criteria

- In a git checkout with commits past the last tag, `--version` shows the
  `-N-g<sha>` suffix; with uncommitted changes it shows `-dirty`.
- On a clean tagged checkout, `--version` shows the bare semver (describe == tag).
- `cargo build` from a `.git`-less source tree succeeds and shows the fallback.
- The `agent-runtime-cli` version integration test stays green.
- DEVELOPMENT.md required checks, `completion-asset-audit`,
  `third-party-artifacts`, and locked-build all pass with no new dependency.

## Validation plan

- `cargo test -p agent-runtime-cli` (version test).
- Manual: build, run `<bin> --version` and `-V`; make a throwaway edit and
  confirm the `-dirty` suffix appears.
- Full required checks per DEVELOPMENT.md (the four known CI gates) before PR.
- Rollout touches every binary crate, so confirm the completion flag-parity and
  asset audits cover all wired crates; a `code-review-quick-pass` on the
  mechanical wiring is sufficient (additive, no new dependency).

## Risks and guardrails

- Rebuild cascade on HEAD change — mitigated by leaf-crate isolation
  (Decision 4).
- crates.io publish — `nils-build-info` must package cleanly with its `build.rs`
  and the `.git`-absent fallback; guardrail: dry-run
  `cargo publish -p nils-build-info --dry-run`.
- Reproducible builds — preserved by omitting any wall-clock timestamp
  (Decision 7).

## Execution

- Recommended plan: docs/plans/cli-version-git-metadata/cli-version-git-metadata-plan.md
- Recommended execution state: docs/plans/cli-version-git-metadata/cli-version-git-metadata-execution-state.md
- Status: plan generated; tracked by a plan-tracking issue.
- Next-task source: this document.

## Retention intent

- Plan-scoped. Clean up the `docs/plans/cli-version-git-metadata/` folder after
  execution lands and the PR merges, unless the content is promoted into a
  versioning runbook.

## Read-first references

- Root `Cargo.toml` `[workspace.package]`.
- `crates/agent-runtime-cli/tests/integration/cli.rs` (version test).
- `crates/cli-template/src/main.rs` (version wiring pattern).
- `DEVELOPMENT.md` (required checks).

## Recommended next artifact

- A plan (`*-plan.md`) sequencing: create `nils-build-info` -> wire all binary
  crates that render `--version` -> validation -> PR, tracked via
  `create-plan-tracking-issue`.
