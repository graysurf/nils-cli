# Repairing a PR after its head changes

How to keep one pull request — and one review timeline — across a CI repair
loop, instead of closing it and opening a replacement.

This is the workflow #1398 was filed about. The capability already existed; what
was missing was this page.

## The property that makes it feel impossible

Delivery evidence is bound to an exact commit, tree digest, and delivery
attempt. That is deliberate: evidence that survived an amend would no longer
describe the code being merged. So after `git commit --amend` and a
`--force-with-lease` push, the evidence bound to the old head is stale, and
`pr deliver` refuses it with `test_first_evidence_subject_mismatch` or
`test_first_evidence_provider_head_mismatch`.

The reflex is to close the PR and open a new one. That is not necessary, and it
throws away the review timeline, the review-loop ledger, and every thread.

## The loop

The baseline is **never** replaced. `bind-delivery` appends a new attestation
for the new head; the old attempt stays auditable.

```bash
# 1. repair, amend, and re-sign
git commit --amend        # via semantic-commit in this repo
git-cli push --force-with-lease

# 2. re-bind evidence to the new head and re-verify
test-first-evidence record-final --out "$EVIDENCE_DIR" \
  --command 'bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast' \
  --status pass --scope focused
test-first-evidence bind-delivery --out "$EVIDENCE_DIR" --project-path .
test-first-evidence verify --out "$EVIDENCE_DIR" --project-path . --format json

# 3. re-deliver against the SAME pr — deliver adopts it, it does not recreate
forge-cli pr deliver --kind feature --title "$TITLE" --body-file "$PR_BODY" \
  --base main --test-first-evidence "$EVIDENCE_DIR" --no-merge
```

`pr deliver` looks up the open PR on the head branch and **adopts** it, recording
an `adopt` step instead of a `create` step. The PR number, author, labels, draft
state, and review timeline are untouched.

## Ordering rules that cannot be repaired after the fact

**Record the review-loop observation before you push the repair.** An
observation can only be appended at the current provider head, so history cannot
be backfilled; and a `fixed` disposition requires a *repaired* head, so a finding
cannot be declared fixed at the head where it was first recorded. Together these
force: observe findings as `open` at the reviewed head → repair → push → observe
them as `fixed` at the new head. Doing every repair first and then trying to
write the history is unrecoverable — the pre-repair head is gone.

**Pin the head at merge.** Pass `--expected-head <sha>` so the merge is a
compare-and-swap against the head you verified. Without it a concurrent push can
land between your last check and the merge.

## Checks after a force-with-lease

A new head starts with no checks registered. The provider needs time to register
them, and during that window `gh pr checks --required` reports "no required
checks reported".

`pr wait-checks` and `pr merge` treat that empty snapshot as **not yet checked**
rather than as a pass: the wait polls through the window, and both fail closed
with `checks_not_registered` if nothing ever appears. This is lock-down rule 8 —
see the spec's "Absence is not success". Before that rule existed, a re-delivery
inside the registration window could report green having run no CI at all.

If the repository genuinely configures no checks, say so explicitly:

```bash
forge-cli pr merge "$PR" --method squash --expected-head "$SHA" \
  --allow-no-checks --allow-no-checks-reason 'this repository configures no CI'
```

The reason is recorded in the merge envelope as `no_checks_override_reason`, and
it is the only durable record that a merge happened without CI evidence.

## Reducing the number of iterations

Broad CI repair is slow when each run reports only the next failure. Use
`cargo nextest --no-fail-fast` so one run reports every remaining failure. That
does not replace anything above; it just makes the loop converge in fewer trips.

## Links

- [`../specs/forge-cli-spec-v1.md`](../specs/forge-cli-spec-v1.md) — lock-down
  rules, including rule 8 and the `checks_not_registered` mapping.
- Back to crate docs: [`../README.md`](../README.md)
