<!-- agent-kit:specialist-review-report:v1 -->
## Review Report

- Reviewable: PR #1
- Lens: testing
- Lens verdict: findings
- Scope: golden template shape
- Evidence reviewed: golden fixture

| Finding | Severity | Confidence | Evidence | Recommendation |
| --- | --- | ---: | --- | --- |
| Missing test for new branch | high | 0.85 | src/lib.rs:42 — evidence for Missing test for new branch | Add a unit test exercising the new branch. |
| Function is too long | medium | 0.72 | src/util.rs:10 — evidence for Function is too long | Split the helper into smaller functions. |
