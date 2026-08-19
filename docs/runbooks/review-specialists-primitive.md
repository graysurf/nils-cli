# Review Specialists Primitive

`review-specialists` is the deterministic CLI primitive for the
`code-review-specialists` workflow. The workflow still owns reviewer judgment,
specialist selection, red-team narrative, and provider decisions. The CLI only
validates, normalizes, merges, renders, bundles, and scopes local review data.

## Boundary

The CLI never:

- runs specialist prompts or calls model providers;
- spawns subagents;
- posts GitHub or GitLab comments;
- opens issues or pull requests;
- makes merge, close, approve, or request-changes decisions.

Use `review-evidence` for retained review evidence records and use the owning
provider workflow for live PR or issue actions.

## Finding Input

Each finding is one JSON object per line:

```json
{"severity":"high","confidence":0.82,"path":"src/api/users.rs","line":42,"category":"api-contract","summary":"Response shape changed without migration guidance.","evidence":"Diff removes a field while callers still read it.","recommendation":"Add compatibility handling or update callers and tests.","specialist":"api-contract","test_suggestion":"Add a contract test."}
```

Required fields are `severity`, `confidence`, `path`, `summary`, `evidence`,
`recommendation`, and `specialist`. Optional fields are `line`, `category`,
`fingerprint`, `root_cause_fingerprint`, and `test_suggestion`.

Severity aliases normalize to `critical`, `high`, `medium`, `low`, and `info`.
Confidence must be `0.0..=1.0`. Unknown fields are rejected so fixture drift is
visible during validation. The closed specialist values are `api-contract`,
`data-migration`, `maintainability`, `performance`, `quick`, `red-team`,
`security`, and `testing`; `quick` is accepted as a finding producer but is not
part of the scope command's multi-specialist initial wave.

## Commands

```bash
review-specialists scope --base main --format json
review-specialists validate --input findings.jsonl --format json
review-specialists merge --input findings.jsonl --summary-out review.md
review-specialists merge --mode delivery --input findings.jsonl --format json
review-specialists render --profile issue-body --input findings.merged.json \
  --repo sympoies/nils-cli --ref HEAD --out issue.md
review-specialists bundle --input findings.jsonl --out-dir target/review-specialists/bundle \
  --profile issue-body
```

`scope` shells out to local `git` and emits changed files, diff-line counts,
stack signals, test framework signals, suggested specialists, forced
specialists, small-diff skip metadata, and red-team trigger metadata.

`validate`, `merge`, and `bundle` default to advisory mode. Advisory mode keeps
the compatibility behavior: it deduplicates by explicit `fingerprint` when
present and otherwise computes one from `path`, `line`, `category`, and
`summary`.

Delivery mode requires every row to declare a stable
`<category>:<component>:<invariant>` fingerprint. Its first segment must match
the finding category. An optional `root_cause_fingerprint` groups multiple lens
observations under one lifecycle identity without dropping their `source_rows`.
Incompatible reuse fails as `review_fingerprint_collision`; missing explicit
identity fails as `review_fingerprint_required`. Delivery output uses the v2
finding/merge schemas and exposes `lifecycle_fingerprint` for the downstream
review-loop ledger. The highest-confidence row becomes the primary finding,
with confirming specialists retained in deterministic order.

The downstream observation array may attach an explicit `status` or
`disposition` of `open`, `fixed`, `accepted`, `preference`, or `follow-up` to a
lifecycle fingerprint. Omitting a previously open finding on the same head is
not a disposition; `forge-cli` requires the explicit field or a repaired head.

`bundle` writes:

- `findings.normalized.jsonl`
- `findings.merged.json`
- `specialist-review.md`
- one optional profile artifact such as `issue-body.md`

Invalid input fails before bundle artifacts are written.

## Render Profiles

- `terminal`: compact summary for chat or local terminal output.
- `report`: full specialist report sections.
- `issue-body`: follow-up-oriented issue body.
- `pr-comment`: review-oriented comment body.
- `evidence`: compact JSON summary for evidence linking.

Provider profiles are local renderers only. They do not post anything.

## Downstream Skill Adoption

`code-review-specialists` can replace its Python helper by calling:

```bash
review-specialists scope --base <ref> --format json
review-specialists merge --input <findings.jsonl> --summary-out <report.md> --format json
```

The skill should continue to author findings, choose specialist lenses, decide
whether red-team review is warranted, and route any live provider actions
through the appropriate provider workflow.
