# forge-cli

Provider-neutral CLI for remote forge operations (personal inbox discovery,
PR/MR lifecycle, Issue lifecycle, CI wait, and repository label catalog
maintenance). Two backends ship together: GitHub (wraps `gh`) and GitLab
(wraps `glab`). Adopts `cli-output-contract-v1` from day one.

## Read first

- Docs index: [docs/README.md](docs/README.md)
- Contract: [docs/specs/forge-cli-spec-v1.md](docs/specs/forge-cli-spec-v1.md)
- Op catalog: [docs/specs/forge-cli-ops-v1.yaml](docs/specs/forge-cli-ops-v1.yaml)
- Workspace envelope contract:
  [`/docs/specs/cli-output-contract-v1.md`](../../docs/specs/cli-output-contract-v1.md)

## Quick start

```sh
cargo run -p nils-forge-cli -- --help
cargo run -p nils-forge-cli -- inbox status --format json
cargo run -p nils-forge-cli -- auth status --format json
cargo run -p nils-forge-cli -- label audit --catalog labels.yaml --format json
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
`--hostname <host>` to `glab api`; set `FORGE_CLI_INBOX_GITLAB_HOST` for a
default self-managed host, or use `--gitlab-host` for a per-command override.
`status` reports bounded counts, and `next` returns a ranked bounded subset
without mutating PRs, issues, merge requests, or todos.

For VPN-dependent GitLab hosts, keep daily mixed-provider usage responsive by
requiring a readiness check and bounding GitLab backend calls:

```sh
forge-cli inbox list --format json \
  --gitlab-host gitlab.example.com \
  --gitlab-vpn required \
  --gitlab-vpn-check tcp:gitlab.example.com:443 \
  --provider-timeout 20s
```

When the VPN check fails, mixed-provider mode still returns GitHub results with
a GitLab `vpn_unavailable` provider row and warning. `--provider github`
intentionally skips GitLab. `--provider gitlab` fails when GitLab is selected
but VPN-unavailable or timed out. Add `--strict-providers` for automation that
must fail any partial provider failure.

`--gitlab-vpn-check cmd:<program>` delegates readiness to a local script, and
`--gitlab-vpn-check openvpn` verifies local OpenVPN CLI/profile prerequisites
without starting or stopping VPN. OpenVPN profile paths are local-only
configuration and are redacted from JSON, warnings, issue records, docs, and
cache files. Install optional OpenVPN CLI support with `brew install openvpn`.

Successful provider reads write local cache snapshots. Stale fallback is
opt-in:

```sh
forge-cli inbox list --format json --cache-fallback --cache-max-age 30m
```

Cached fallback items are marked with `stale` metadata and the provider row
remains `ok=false`, so consumers can distinguish stale context from live data.

### Reason filter (`--kind`) vs item-type filter (`--item-type`)

`--kind` selects inbox *reasons* — why an item should appear (`review`,
`assigned`, `todo`, `authored`, `involved`). `--item-type` selects *result
classes* — pull/merge requests, issues, or all items. They are independent:

```sh
# default: all reasons, all item types
forge-cli inbox list --format json

# pull/merge requests only (skips GitHub issue searches and GitLab issue API calls)
forge-cli inbox list --item-type pr --format json

# issues only (skips PR searches; GitHub review-requested is dropped)
forge-cli inbox list --item-type issue --format json

# review-requested PRs only
forge-cli inbox list --kind review --item-type pr --format json
```

`--item-type` defaults to `all`. Dry-run output reflects the pruned query plan:

```sh
forge-cli --dry-run --format json inbox list --item-type pr
```

GitLab `todos` are classified by `target_type` (or the target URL); todos whose
target cannot be classified appear only in `--item-type all` mode.

## Label catalog operations

`forge-cli label` keeps provider labels aligned with a caller-owned YAML/JSON
catalog. The catalog remains outside `nils-cli`; `forge-cli` only validates,
audits, and applies the provider operations.

```sh
forge-cli label list --format json
forge-cli label audit --catalog manifests/forge-labels.yaml --format json
forge-cli --dry-run label ensure --catalog manifests/forge-labels.yaml --update-existing --format json
```

`label audit` reports missing catalog labels, color / description drift, and
unknown shared labels. `label ensure` creates missing labels and updates
existing color / description drift only with `--update-existing`; it never
deletes labels or renames labels by default.

`pr create` and `pr deliver` accept repeated `--label <name>` flags. Add
`--label-catalog <path> --strict-labels` when the caller wants `forge-cli` to
reject unknown, not-applicable, or mutually exclusive labels before a PR/MR is
opened.

### Latency notes

Provider adapters and independent query families run concurrently, so
default-mode latency is bounded by the slowest single backend call rather than
their sum. Identity lookup is only issued when a remaining GitLab query needs
it. Manual smoke timings (provider/network dependent, not a CI assertion):

```sh
time forge-cli --provider github --format json inbox list --limit 30
time forge-cli --provider github --format json inbox list --limit 30 --item-type pr
time forge-cli --provider gitlab --gitlab-host gitlab.example.com --format json inbox list --limit 30
time forge-cli --format json inbox list --gitlab-host gitlab.example.com --limit 30
```

Wall-clock latency depends on `gh`/`glab` and remote API responsiveness; treat
these timings as delivery evidence, not deterministic budgets.

## GitHub checks compatibility

Starting with `nils-cli` `0.17.0`, GitHub `pr checks` calls request only the
`gh 2.92.0` supported JSON fields. Required-check gates use an explicit
`gh pr checks --required` snapshot instead of the removed `isRequired` JSON
field, so `pr checks`, `pr wait-checks`, `pr merge`, and `pr deliver` share the
same compatibility path.
