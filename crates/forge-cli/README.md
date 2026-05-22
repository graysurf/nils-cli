# forge-cli

Provider-neutral CLI for remote forge operations (personal inbox discovery,
PR/MR lifecycle, Issue lifecycle, CI wait). Two backends ship together: GitHub
(wraps `gh`) and GitLab (wraps `glab`). Adopts `cli-output-contract-v1` from
day one.

## Read first

- Contract: [docs/specs/forge-cli-spec-v1.md](docs/specs/forge-cli-spec-v1.md)
- Op catalog: [docs/specs/forge-cli-ops-v1.yaml](docs/specs/forge-cli-ops-v1.yaml)
- Plan: [`/docs/plans/forge-cli/forge-cli-plan.md`](../../docs/plans/forge-cli/forge-cli-plan.md)
- Workspace envelope contract:
  [`/docs/specs/cli-output-contract-v1.md`](../../docs/specs/cli-output-contract-v1.md)

## Quick start

```sh
cargo run -p nils-forge-cli -- --help
cargo run -p nils-forge-cli -- inbox status --format json
cargo run -p nils-forge-cli -- auth status --format json
cargo run -p nils-forge-cli -- pr deliver --kind feature --dry-run --format json
```

`forge-cli` does NOT introduce a `--json` boolean flag. Use
`--format text|json` exclusively.

## Inbox discovery

`forge-cli inbox` is a read-only personal work inbox for agents, scheduled jobs,
and Alfred-style consumers:

```sh
forge-cli inbox list --format json
forge-cli inbox status --provider gitlab --gitlab-host gitlab.example.com --format json
forge-cli inbox next --limit 5 --format json
```

With no `--provider`, inbox queries GitHub and GitLab and keeps successful
provider results when another provider fails. GitLab inbox calls always pass
`--hostname <host>` to `glab api`; use `--gitlab-host` for self-managed hosts.
`status` reports bounded counts, and `next` returns a ranked bounded subset
without mutating PRs, issues, merge requests, or todos.

## GitHub checks compatibility

Starting with `nils-cli` `0.17.0`, GitHub `pr checks` calls request only the
`gh 2.92.0` supported JSON fields. Required-check gates use an explicit
`gh pr checks --required` snapshot instead of the removed `isRequired` JSON
field, so `pr checks`, `pr wait-checks`, `pr merge`, and `pr deliver` share the
same compatibility path.
