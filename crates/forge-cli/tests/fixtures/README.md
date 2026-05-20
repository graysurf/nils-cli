# forge-cli test fixtures

This tree holds the canonical `gh` / `glab` responses every forge-cli integration test replays against the stub binaries injected via `FORGE_CLI_GH_BIN` / `FORGE_CLI_GLAB_BIN`. The fixtures are split per-op (e.g. `pr_create/`, `pr_checks/`) and per-provider (`github/`, `gitlab/`).

## Redaction policy

Token-shaped strings MUST NOT appear anywhere under this tree. `scripts/ci/forge-cli-fixture-lint.sh` runs in CI's docs-only lane and fails the build on any match for these patterns:

| Pattern (regex)             | Source                                 |
| --------------------------- | -------------------------------------- |
| `gh[ps]_[A-Za-z0-9_]{16,}`  | GitHub personal / server tokens        |
| `ghr_[A-Za-z0-9_]{16,}`     | GitHub refresh tokens                  |
| `gho_[A-Za-z0-9_]{16,}`     | GitHub OAuth tokens                    |
| `glpat-[A-Za-z0-9_-]{16,}`  | GitLab personal access tokens          |
| `Bearer [A-Za-z0-9._-]{16,}`| Generic bearer auth headers (e.g. JWT) |

Replace every occurrence with `<redacted-token>` (or `<redacted-jwt>` for bearer-style headers). When you need a token-shaped placeholder to exercise downstream parsing without tripping the lint, use shorter shapes (under 16 characters) — they're below the lint's threshold and stay obviously synthetic.

## Adding a new fixture

1. Drop the file under `crates/forge-cli/tests/fixtures/<provider>/<op>/<name>.<ext>`.
2. Confirm the placeholder values use the same redaction markers as existing fixtures.
3. Run `bash scripts/ci/forge-cli-fixture-lint.sh` locally before committing.
4. Reference the fixture from the matching integration test via `include_str!(...)`.

## Negative regression test

`crates/forge-cli/tests/integration/fixture_lint.rs` writes a synthetic
GitHub-token-shaped string (e.g. `ghp_` followed by 20+ alphanumerics) into a
tempdir and re-invokes the lint script against that tempdir. The lint must
exit non-zero and surface the file path + line in stderr; the test asserts
both. The actual token-shaped string is constructed in code so this README
never contains one itself.
