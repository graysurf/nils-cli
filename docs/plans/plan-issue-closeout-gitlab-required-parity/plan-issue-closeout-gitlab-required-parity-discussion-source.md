# plan-issue closeout GitLab `Required` column parity — Source

| Field              | Value                                                                                                                      |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| Status             | Ready for implementation                                                                                                   |
| Date               | 2026-05-26                                                                                                                 |
| Source             | sympoies/nils-cli#557 — GitLab adapter renders `Required: unknown`; follow-up to the GitHub-side render fix landed in #563 |
| Intended next step | Implement the GitLab adapter change in a single small PR that closes #557                                                  |

## Purpose

The closeout-comment renderer for `plan-issue record close` now distinguishes
five `Required` column labels (`none required`, `pass (N)`, `fail (N)`, `none`,
`unknown`) after the fix in
[`plan-issue-closeout-required-check-rendering`][rcr-source] / sympoies/nils-cli#563.
The GitHub adapter feeds that renderer with a true `required_state` /
`required_count` pair derived from `gh pr checks --required`.

The GitLab adapter at
`crates/plan-issue-cli/src/forge_cli_adapter.rs:343-356` still returns
`required_state: None, required_count: None, non_required_failures: []`
for every PR, because GitLab has no first-class required-check concept and
`glab` exposes only the rolled-up pipeline state. That `None` collapses to
`unknown` in the renderer, so every GitLab PR row in a closeout comment
reads `Required: unknown` even when the pipeline is green and there is no
required-check rule to satisfy.

The fix is to make the GitLab adapter return the same shape the
GitHub side now uses for branches without a required-check rule. At
the `PrMergeSummary` layer (`crates/plan-issue-cli/src/github.rs:76`)
`required_state` is an `Option<String>`; the rendering chain converts
that via `check_status_from_state` (`execute.rs:595-604`) to
`Option<CheckStatus>` before reaching the renderer. So the adapter
returns `required_state: Some("success".to_string()), required_count:
Some(0)`, the renderer's `required_check_label` then sees
`(Some(CheckStatus::Pass), Some(0))` and produces the `none required`
label. The semantic intent of #502
(close gate keys on required checks only; non-required failures never
block) is preserved — see Decision 2 for the explicit semantic shift this
implies and why it is consistent with #502.

## Confirmed facts

- The renderer's label table at
  `crates/plan-issue-cli/src/lifecycle_record.rs:2154-2160` already maps
  `(Some(CheckStatus::Pass), Some(0))` → `"none required"`. A unit test
  at `lifecycle_record.rs:3223-3260` exercises all five label branches
  including this one. No render-layer change is needed for this work. [F1]
- The GitLab adapter `pr_merge_summary` at
  `crates/plan-issue-cli/src/forge_cli_adapter.rs:343-356` hard-codes
  `required_state: None, required_count: None, non_required_failures:
  Vec::new()` with an inline comment ("GitLab has no first-class
  required-check concept …"). [F2]
- The close-gate path at
  `crates/plan-issue-cli/src/lifecycle_record.rs:2446-2469` matches on
  `pr.required_state`:
  - `Some(CheckStatus::Fail)` — blocks close as a required-check failure.
  - `Some(CheckStatus::Pass | CheckStatus::None)` — treats the PR as
    "required checks resolved cleanly (including `required_count == 0`)".
    Aggregate `checks` value is informational only.
  - `None` — conservative fallback: aggregate `pr.checks == Fail`
    blocks close unless `allow_non_required_check_failure` is set. The
    inline comment explicitly names "GitLab today" as the canonical
    triggering case. [F3]
- sympoies/nils-cli#512 ("fix(plan-issue-cli): record close gates on
  required checks only (#502)", merged 2026-05-25) introduced the
  required-vs-non-required split. Its source doc
  (`docs/plans/plan-issue-close-non-required-checks/plan-issue-close-non-required-checks-discussion-source.md`,
  R-2) explicitly leaves GitLab parity to a follow-up:
  "_when the adapter cannot resolve a meaningful `required_count`, fall
  back to the existing aggregate `state` semantics_". [F4]
- sympoies/nils-cli#563 ("feat(plan-issue-cli): repair closeout Required
  column rendering", merged 2026-05-26 12:58 UTC; commit `f2666c4`) is on
  `main`. The supporting docs PRs #558 and #564 are merged. Issue #557's
  "Next" prerequisite (1) is satisfied. [F5]
- `crates/plan-issue-cli/src/forge_cli_adapter.rs:747-766` already has a
  fixture-backed unit test `pr_merge_summary_composes_view_and_checks`
  that asserts the `state` / `merged` / `merge_sha` / `checks` fields on
  a stubbed GitLab response. The test is the natural extension point for
  asserting `required_state` / `required_count` / `non_required_failures`
  on the new return shape. [F6]
- The historical closeout payload on
  <https://github.com/sympoies/nils-cli/issues/541#issuecomment-4543937296>
  shows `required_state: null` for GitHub PRs; the equivalent GitLab
  evidence is the same observation against any GitLab closeout produced
  before the fix lands. Historical closeout records are immutable in the
  provider; the change applies only to records produced after the fix. [F7]

## Decisions

1. **Render-only fix at the GitLab adapter boundary.** Change the GitLab
   branch of `pr_merge_summary` to return:

   ```rust
   PrMergeSummary {
       state,
       merged,
       merge_sha,
       checks,
       required_state: Some("success".to_string()),
       required_count: Some(0),
       non_required_failures: Vec::new(),
   }
   ```

   `check_status_from_state` (`execute.rs:595-604`) converts the
   `"success"` string to `CheckStatus::Pass` before the renderer's
   `required_check_label` runs, and that helper already produces
   `none required` for `(Some(CheckStatus::Pass), Some(0))`, so no
   render-layer change is needed. The label is the same one the GitHub
   adapter now emits for a branch without a required-check rule, which
   keeps the cross-provider closeout output consistent.

2. **Accept the close-gate semantic shift; it is consistent with #502.**
   Moving from `None` to `Some(Pass) + Some(0)` flips which match arm in
   `lifecycle_record.rs:2446-2469` GitLab PRs land in:
   - **Before:** `None` → conservative fallback. A GitLab pipeline in
     `state=failure` would block close on aggregate `checks=Fail` unless
     `allow_non_required_check_failure` is set.
   - **After:** `Some(Pass)` → "required checks resolved cleanly". A
     failing pipeline no longer blocks close on the GitLab path.

   This is consistent with #502's stated rule that "non-required failures
   never block close" applied to a provider that has no concept of
   required checks. The mitigation for accidental green-light is that
   `summary.checks` still surfaces the aggregate pipeline state to
   callers and downstream tooling — close gate is decoupled from that
   field, but the field itself is unchanged. Issue #557's body endorses
   the same shift ("minimum-cost way to land on the same label"). The
   doc surfaces the change explicitly so the next reader does not need
   to re-derive it from the diff.

3. **Annotate the adapter, retire the stale comment.** The inline comment
   at `forge_cli_adapter.rs:343-347` currently says "we leave the required
   fields at `None`/empty so the close gate falls back to the aggregate
   `checks` value (matching pre-#502 GitLab behavior)". Replace it with a
   short comment stating the new contract: GitLab has no required-check
   concept, so the adapter reports zero required checks (the same shape
   the GitHub adapter returns for a branch without a required-check
   rule), and the close gate treats this as a clean resolve per #502.
   Reference #557 for context.

4. **Test at the adapter boundary, not the renderer.** Extend or split
   `pr_merge_summary_composes_view_and_checks` at
   `forge_cli_adapter.rs:748-766` so it also asserts:

   ```text
   summary.required_state.as_deref() == Some("success")
   summary.required_count == Some(0)
   summary.non_required_failures.is_empty()
   ```

   The renderer's existing five-branch test
   (`lifecycle_record.rs:3223-3260`) already covers
   `(Some(Pass), Some(0)) → "none required"`. No new render-layer test
   is needed — chain of trust via the adapter test plus the existing
   label test covers the full path.

5. **No schema change.** `closeout.v1` and the `LinkedPrEvidence` /
   `PrMergeSummary` Rust types are unchanged. The `Option<CheckStatus>`
   / `Option<u32>` field shapes carry the new values without any
   serialization migration. Historical closeout records continue to
   parse, and their hex-encoded payloads do not need re-encoding.

6. **Scope stays inside the GitLab adapter.** No changes to the GitHub
   adapter, the closeout-comment renderer, the lifecycle-record close
   gate, or the `PrMergeSummary` struct. No new dependencies.

## Scope

- `crates/plan-issue-cli/src/forge_cli_adapter.rs`:
  - Replace the `(None, None, [])` return triple in `pr_merge_summary`
    with `(Some("success".to_string()), Some(0), Vec::new())`. The
    string is converted to `CheckStatus::Pass` downstream of the
    adapter by `execute.rs::check_status_from_state`.
  - Replace the inline comment per Decision 3.
  - Extend `pr_merge_summary_composes_view_and_checks` (or add a sibling
    test in the same `#[cfg(test)] mod tests` block) to assert the new
    triple.

## Non-scope

- The GitHub adapter (`crates/plan-issue-cli/src/github.rs`). The render
  fix landed in #563; this PR does not touch it.
- The renderer (`crates/plan-issue-cli/src/lifecycle_record.rs`). The
  five-branch label table already produces `none required` for the new
  GitLab triple.
- The `closeout.v1` payload schema. Wire format is unchanged.
- The `LinkedPrEvidence.checks` aggregate state. Unrelated to the
  required-check resolution; left as today's `summary.checks`.
- Backfill of historical closeout comments. They remain in their posted
  form.
- The close-gate logic in `lifecycle_record.rs:2446-2469`. The match arms
  stay as today; only which arm GitLab PRs land in changes. Decision 2
  records the semantic implication explicitly.

## Implementation boundaries

- The change is a single struct-literal swap plus a comment rewrite plus
  one extended test. No new helper functions, no module reorganisation,
  no new dependencies.
- The new adapter-layer values must reuse the canonical `"success"`
  string (the same value `pr_required_summary` returns from `gh pr
  checks --required` on GitHub) so `check_status_from_state` maps it to
  `CheckStatus::Pass`; do not introduce a new string or a
  GitLab-specific marker.
- The test must run without `glab` on PATH (uses the existing fake-process
  adapter wiring at `adapter_with(vec![...])`).

## Requirements

- **R1.** `pr_merge_summary` on the GitLab adapter returns
  `required_state: Some("success".to_string())`,
  `required_count: Some(0)`, and an empty `non_required_failures` vector
  for every PR, regardless of pipeline outcome. Downstream of the
  adapter, `check_status_from_state` converts that to
  `CheckStatus::Pass` so the renderer's `required_check_label` hits
  the `(Some(Pass), Some(0)) → "none required"` arm.
- **R2.** A closeout comment posted by `plan-issue record close` against
  a GitLab PR renders that PR's `Required` column as `none required`
  via the existing `required_check_label` mapping.
- **R3.** The existing close-gate semantics are unchanged at the code
  level; GitLab PRs simply land in the `Some(CheckStatus::Pass |
  CheckStatus::None)` arm of `lifecycle_record.rs:2450` instead of the
  `None` arm at `:2455`.
- **R4.** A unit test exercises R1 without invoking the real `glab`
  binary, using the adapter's existing fake-process wiring.

## Acceptance criteria

- **AC-1.** `cargo test -p plan-issue-cli` is green from the workspace
  root.
- **AC-2.** A new (or extended) unit test in `forge_cli_adapter.rs`'s
  test module asserts the full new triple for
  `pr_merge_summary_composes_view_and_checks` (or a sibling test):
  `summary.required_state.as_deref() == Some("success")`,
  `summary.required_count == Some(0)`,
  `summary.non_required_failures.is_empty()`.
- **AC-3.** The renderer test
  `required_check_label_emits_five_distinct_branches`
  (`lifecycle_record.rs:3223-3260`) continues to pass unmodified — the
  `(Some(Pass), Some(0)) → "none required"` branch is the one this work
  relies on.
- **AC-4.** The diff does not modify `lifecycle_record.rs`,
  `github.rs`, or any `closeout.v1` serialization site.
- **AC-5.** The new comment at `forge_cli_adapter.rs:343-347` cites

  #557 (and references the new contract: GitLab reports zero required
  checks; close-gate treats as clean resolve per #502).

## Validation plan

1. `cargo test -p plan-issue-cli` from the workspace root.
2. `cargo clippy -p plan-issue-cli --all-targets --all-features -- -D
   warnings` for clippy hygiene.
3. `cargo build -p plan-issue-cli --locked` (Cargo.lock locked-build CI
   gate, captured in [F-nils-cli-new-crate-ci]).
4. `bash scripts/ci/nils-cli-checks-entrypoint.sh --docs-only` for the
   docs-only hygiene gate (rumdl fmt / third-party / completion-asset
   audit don't apply to this diff, but the docs-only entrypoint covers
   the markdown-lint pass over the new source doc).
5. Manual (optional): `cargo run -p plan-issue-cli -- record close
   --provider gitlab --issue <n> --dry-run` against a representative
   GitLab repo with one merged MR linked; eyeball the rendered table for
   the new `none required` label. Skip if no GitLab plan-issue is
   handy — AC-2 plus AC-3 already chain to the same result.

## Findings table

| ID  | Source                                                    | Disposition                                                                                     |
| --- | --------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| F-1 | `crates/plan-issue-cli/src/forge_cli_adapter.rs:343-356`  | In scope — replace `(None, None, [])` with `(Some(Pass), Some(0), [])` plus comment refresh     |
| F-2 | `crates/plan-issue-cli/src/forge_cli_adapter.rs:748-766`  | In scope — extend the fixture-backed test to assert the new triple                              |
| F-3 | `crates/plan-issue-cli/src/lifecycle_record.rs:2154-2160` | Confirmed read-only — label table already maps the new triple to `none required`                |
| F-4 | `crates/plan-issue-cli/src/lifecycle_record.rs:2446-2469` | Confirmed read-only — semantic shift accepted per Decision 2; no code change needed             |
| F-5 | sympoies/nils-cli#502 / #512 / #563                       | Read-first context — close-gate contract and GitHub render fix; this PR is the GitLab follow-up |

## Risks and guardrails

- **R-1. Silent close-gate green-light on a failing GitLab pipeline.**
  After this change, a GitLab MR whose pipeline rolled up to `state=
  failure` will land in the "required checks resolved cleanly" arm and
  no longer block close on aggregate checks. Mitigation: the close gate
  was already in this state for GitHub PRs without a required-check
  rule after #502; treating GitLab — which has no required-check concept
  at all — the same way is the documented #502 contract. The
  `summary.checks` field still reports the aggregate state for
  downstream tooling, and operators with a strict "must be green to
  close" policy can set the corresponding pipeline as required at the
  GitLab project level (out of scope here). Decision 2 surfaces this
  trade-off so the next reader can audit it without re-deriving it from
  the diff.
- **R-2. Cosmetic regression on a downstream consumer that parses the
  `Required` column.** The free-form Markdown label was `unknown` and
  is now `none required`. Mitigation: the label is not part of
  `closeout.v1` (the wire field stays
  `Option<CheckStatus>` / `Option<u32>`); no known automation scrapes
  it. Same mitigation as #563's R-2.
- **R-3. Future GitLab CI capability change.** If `glab` later grows a
  required-check concept, the adapter will need to plumb a real
  `required_count`. Mitigation: keep the new comment short and pointer
  at #557 so a future reader can audit the assumption when refactoring.

## Execution

- Recommended plan:
  `docs/plans/plan-issue-closeout-gitlab-required-parity/plan-issue-closeout-gitlab-required-parity-plan.md`
- Recommended execution state:
  `docs/plans/plan-issue-closeout-gitlab-required-parity/plan-issue-closeout-gitlab-required-parity-execution-state.md`
- Status: ready
- Next-task source: this document; the change is single-PR sized and
  does not require a separate plan/execution-state artifact unless the
  delivery surface specifically asks for them. If the implementer opens
  a lightweight plan-tracking issue beyond #557, it can be slotted into
  the recommended plan path above.

## Retention intent

Cleanup after execution. This is a one-line struct-literal swap plus a
test extension; once the fix lands and #557 closes, the source document
can be dropped without losing durable knowledge. The cross-link from
[`plan-issue-closeout-required-check-rendering`][rcr-source] already
captures the architectural rationale for the five-label set.

## Read-first references

- [`plan-issue-closeout-required-check-rendering`
  source][rcr-source] — the GitHub-side render fix this work is the
  GitLab follow-up of. Explains the five-branch label table and the
  `required_check_label` helper.
- `docs/plans/plan-issue-close-non-required-checks/plan-issue-close-non-required-checks-discussion-source.md`
  — required-vs-non-required close-gate contract (#502); R-2 in that
  doc's Risks names this exact GitLab follow-up.
- `crates/plan-issue-cli/docs/specs/issue-backed-plan-record-contract-v2.md`
  — `closeout.v1` schema; confirms `required_state` stays
  `Option<CheckStatus>` over the wire.

## Recommended next artifact

- Single small PR closing sympoies/nils-cli#557. Branch:
  `feat/plan-issue-closeout-gitlab-required-none` (or
  `fix/plan-issue-557-gitlab-required-none` — `fix/` is defensible
  because this is the GitLab leg of the same bug #557 frames as a
  rendering defect). PR body links this source doc under `Read First`,
  cites #557 / #502 / #563, and ships the adapter change plus the
  extended test in one commit.

[rcr-source]: ../plan-issue-closeout-required-check-rendering/plan-issue-closeout-required-check-rendering-discussion-source.md
[F-nils-cli-new-crate-ci]: /Users/terry/.config/agent-memory/global/feedback_nils_cli_new_crate_ci.md
