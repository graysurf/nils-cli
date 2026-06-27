# nils-agent-out

`agent-out` generates and audits canonical `$AGENT_HOME/out/` artifact paths for agent workflows.

The CLI keeps ad hoc project artifacts under:

```text
$AGENT_HOME/out/projects/<project-slug>/<YYYYMMDD-HHMMSS>-<topic>/
```

It does not install hooks or block arbitrary filesystem writes. Hooks and skills can consume this command later.

## Commands

### `project`

Generate a canonical project-scoped run directory path.

```bash
agent-out project --topic "api smoke" --repo . --mkdir
agent-out project --topic "api smoke" --repo-slug sympoies/nils-cli --format json
agent-out project --topic "api smoke" --format env
```

Options:

- `--topic <TOPIC>`: required run label; sanitized for path safety.
- `--repo <PATH>`: repository used for slug discovery; defaults to the current directory.
- `--repo-slug <OWNER/REPO>`: explicit slug source; `owner/repo` becomes `owner__repo`.
- `--agent-home <PATH>`: agent home root; defaults to `AGENT_HOME`.
- `--mkdir`: create the generated directory.
- `--format path|json|env`: output mode; default is `path`.

Slug precedence:

1. `--repo-slug owner/repo` becomes `owner__repo`.
2. Git `origin` remote under `--repo` or the current directory becomes `owner__repo`.
3. Local fallback becomes `local__<basename>-<short-hash>`.

### `path-for`

Compatibility allocator for rendered `state_out(...)` helper calls that emit
`agent-out path-for --domain ...`. It returns the same canonical project-scoped
run directory shape as `project` while accepting the older domain/topic flags:

```bash
agent-out path-for --domain projects --topic "daily brief" --mkdir
agent-out path-for --domain tools --repo sympoies/nils-cli --format json
agent-out path-for --domain projects --topic "project retro" --format env
```

Options:

- `--domain <DOMAIN>`: required compatibility domain; sanitized for path safety.
- `--topic <TOPIC>`: optional artifact topic. For `--domain projects`, the
  topic is used directly. For other domains, the topic becomes
  `<domain>-<topic>`.
- `--repo <PATH_OR_OWNER/REPO>`: existing paths are used for slug discovery;
  owner/repo-looking values are treated as explicit slugs.
- `--repo-slug <OWNER/REPO>`: explicit slug source; overrides slug discovery.
- `--agent-home <PATH>`: agent home root; defaults to `AGENT_HOME`.
- `--mkdir`: create the generated directory.
- `--format path|json|env`: output mode; default is `path`.

### `audit`

Scan top-level entries under `$AGENT_HOME/out/` and separate canonical or allowlisted roots from noncanonical ad hoc entries.

```bash
agent-out audit
agent-out audit --strict
agent-out audit --format json
```

The MVP allowlist covers the canonical `projects/` root, current home-scope policy roots,
and explicit tool/workflow roots already documented in nils-cli or agent-runtime-kit:

- `projects`
- `agent-browser`
- `api-test-runner`
- `delegate-parallel`
- `image-processing`
- `macos-agent-trace`
- `plan-issue-delivery`
- `plan-issue-sprint-pr`
- `playwright`
- `screen-record`
- `screenshot`
- `semgrep`
- `tests`
- `workspace-shared-audit`
- `workspace-test-cleanup`

New top-level roots should be added deliberately when they become a stable tool contract.

### `completion`

Print generated shell completions:

```bash
agent-out completion zsh > completions/zsh/_agent-out
agent-out completion bash > completions/bash/agent-out
```

## Output Contracts

Human-readable mode is the default. Primary command output goes to stdout; errors go to stderr.

JSON output is opt-in and uses versioned envelopes:

- `cli.agent-out.project.v1`
- `cli.agent-out.path-for.v1`
- `cli.agent-out.audit.v1`

Example:

```json
{
  "schema_version": "cli.agent-out.project.v1",
  "command": "agent-out project",
  "ok": true,
  "result": {
    "path": "/home/user/.agents/out/projects/sympoies__nils-cli/20260511-121314-api-smoke",
    "agent_home": "/home/user/.agents",
    "out_root": "/home/user/.agents/out",
    "repo": "/work/nils-cli",
    "project_slug": "sympoies__nils-cli",
    "topic": "api-smoke",
    "run_id": "20260511-121314-api-smoke",
    "created": false
  }
}
```

## Exit Codes

- `0`: success
- `1`: runtime failure, or audit violations when `audit --strict` is used
- `64`: usage/configuration error, including missing `AGENT_HOME`

## Docs

- [Docs index](docs/README.md)
