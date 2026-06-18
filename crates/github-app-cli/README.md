# github-app-cli

Mint short-lived **GitHub App installation access tokens** so automation can act
under a GitHub App bot identity (for example, having `forge-cli` open issues,
PRs, comments, and merges as `your-app[bot]` instead of a human account).

The crate is a self-contained Rust tool: it signs the App JWT in-process
(`jsonwebtoken`, RS256) and calls the GitHub REST API directly (`reqwest`). It
shells out to no external binary.

## Commands

### `token`

Mint an installation access token (valid ~1 hour).

```bash
github-app-cli token --app-id <ID> --installation-id <ID> --key <path-to.pem>
```

- **Text mode (default):** writes only the raw token to stdout, so it composes
  directly with a token-consuming command:

  ```bash
  GH_TOKEN="$(github-app-cli token)" forge-cli pr deliver ...
  ```

- **JSON mode (`--format json`):** emits a versioned envelope with non-secret
  metadata only (`token_type`, `expires_at`, `repository_selection`,
  `permissions`). The raw token is **never** included in JSON, per the workspace
  output contract.

### `installations`

List the App's installations and their installation IDs (no secrets):

```bash
github-app-cli installations --app-id <ID> --key <path-to.pem>
```

Text mode prints `installation_id<TAB>account<TAB>repository_selection`; JSON
mode wraps the rows in the standard envelope. Use this to discover the
`--installation-id` value an App has on each account/org it is installed on.

### `completion`

```bash
github-app-cli completion <bash|zsh>
```

## Options and environment

Every flag has an environment-variable fallback so a wrapper can stay terse:

| Flag | Env var | Notes |
| --- | --- | --- |
| `--app-id` | `GITHUB_APP_ID` | App ID or Client ID (JWT issuer). |
| `--installation-id` | `GITHUB_APP_INSTALLATION_ID` | `token` only. |
| `--key` | `GITHUB_APP_PRIVATE_KEY_PATH` | Path to the RSA private-key PEM. |
| _(none)_ | `GITHUB_APP_PRIVATE_KEY` | Inline PEM contents; overrides `--key`. |
| `--api-url` | `GITHUB_API_URL` | REST base URL (default `https://api.github.com`; set for GitHub Enterprise). |
| `--format` | — | `text` (default) or `json`. |

Treat the private key like any credential: it can mint tokens indefinitely.
Store it outside version control with tight permissions (`chmod 600`).

## Output and exit codes

Implements the workspace output contract
(`docs/specs/cli-output-contract-v1.md`): `--format text|json`, a versioned
`Envelope`, and BSD sysexits exit codes.

| Code | Meaning |
| --- | --- |
| `0` | success |
| `64` | command-line usage error (e.g. no private key supplied) |
| `65` | invalid input data (unreadable or malformed key) |
| `69` | GitHub API or network unavailable |
| `70` | internal software error |

## Dependencies

- Runtime: none external (HTTP and JWT signing are in-process).
- Build: `jsonwebtoken`, `reqwest` (blocking, rustls), `clap`, `serde`.
