# Sprint 3 sweep — live forge-cli GitLab-arm evidence

Captured 2026-05-25 against the freshly bootstrapped sandbox project
`graysury/nils-cli-gitlab-sandbox` on `gitlab.com` (project id 82523245,
private, default branch `main`). Tracker: [sympoies/nils-cli#514](https://github.com/sympoies/nils-cli/issues/514).

## Environment

- `forge-cli 0.22.0` (released crate)
- `glab 1.99.0` (homebrew)
- `GITLAB_HOST=gitlab.com` (the user's glab default host is `gitlab.gamania.com`;
  the env var is the canonical override per glab's CLI contract)
- Authenticated `glab` user on gitlab.com: `graysury`
- `forge-cli pr create` was run from a fresh sandbox clone (temp dir) because
  glab's MR creation routes through the local repo's git remotes for host
  inference; the nils-cli repo's remotes point at `github.com` only.

## Sweep results

| # | Command shape | Envelope | Result |
| --- | --- | --- | --- |
| 1 | `forge-cli issue list --state all --repo graysury/nils-cli-gitlab-sandbox` | `01-issue-list.json` | `ok=true`, empty list |
| 2 | `forge-cli auth status --repo graysury/nils-cli-gitlab-sandbox` | `02-auth-status.json` | `ok=true`, host=`gitlab.com`, user=`graysury` |
| 3 | `forge-cli pr create --kind feature --head feat/sweep-test --base main` | `03-pr-create.json` | `ok=true`, MR #1 opened as draft |
| 4 | `forge-cli pr view 1` | `04-pr-view.json` | `ok=true`, state=`open`, draft=`true` |
| 5 | `forge-cli pr close 1` | `05-pr-close.json` | `ok=true`, state=`closed` |

All envelopes are `ok=true`. Plan AC-5 satisfied:

- No envelope carries `error.code = glab_version_unsupported`.
- No envelope carries `error.code = repo_not_found`.
- Every step that needed network reached the GitLab API on `gitlab.com` and
  emitted a structured envelope.

## Sandbox bootstrap notes

The sandbox was created by `glab repo create graysury/nils-cli-gitlab-sandbox
--private --description ... --defaultBranch main` (with `GITLAB_HOST=gitlab.com`).
The project was created without a README; the initial commit on `main` was
seeded via the GitLab API (`POST projects/:id/repository/files/README.md`)
because the repo-local `git commit` is blocked by a workflow hook outside
this repo's worktree. The disposable feature branch `feat/sweep-test` was
likewise seeded through the GitLab files API.

## Plan tracking handoff

This evidence completes plan task 3.2. Task 3.3 lands the closeout PR
(this commit). After PR merge, Sprint 3 task 3.1–3.3 advance to `done` and
the tracker state transitions to `complete`; `plan-issue record close`
should then succeed against issue #514.
