# Shell Parity Fixtures

This directory captures baseline output from the Rust CLI entrypoints:
- `plan-issue`
- `plan-issue-local`

## Regenerate fixtures

```bash
bash crates/plan-issue/tests/fixtures/shell_parity/regenerate.sh
```

Normalization rules applied by `regenerate.sh`:
- Replace `${PLAN_ISSUE_HOME}` absolute path with `$PLAN_ISSUE_HOME`.

Fixtures:
- `multi_sprint_guide_dry_run.txt`: `multi-sprint-guide --dry-run` baseline.
- `comment_template_start.md`: extracted start-sprint markdown comment template.
