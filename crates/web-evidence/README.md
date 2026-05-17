# web-evidence

## Overview

`web-evidence` captures a small, deterministic HTTP evidence bundle for agent workflows. It intentionally avoids browser automation,
cookies, auth headers, and raw network logs. The first releaseable primitive is static URL evidence: status metadata, redacted headers, and
a redacted body preview.

## Package vs binary name

| Field        | Value               |
| ------------ | ------------------- |
| Package name | `nils-web-evidence` |
| Binary name  | `web-evidence`      |

## Usage

```text
Capture redacted static HTTP evidence for agent workflows.

Usage: web-evidence <COMMAND>

Commands:
  capture     Capture redacted HTTP metadata and artifact files for one URL
  completion  Print shell completion script

Options:
  -h, --help     Print help
  -V, --version  Print version
```

Examples:

```bash
web-evidence capture https://example.com --out "$AGENT_HOME/out/projects/acme__app/run-1"
web-evidence capture https://example.com --out /tmp/web-evidence --format json
web-evidence capture https://example.com --out /tmp/web-evidence --method head
web-evidence completion zsh
```

## Commands

- `capture URL --out DIR [--format text|json] [--label LABEL] [--method get|head] [--timeout-seconds N] [--max-body-bytes N]
  [--body-preview-bytes N]`: send one HTTP/HTTPS request, create `DIR`, and write a redacted artifact bundle.
- `completion <bash|zsh>`: print clap-generated shell completions.

## Artifact contract

`capture` writes exactly these files under `--out DIR`:

- `summary.json`: versioned summary with status, artifact paths, redaction counts, and error metadata when available.
- `headers.redacted.json`: request/response header names with sensitive values redacted.
- `body-preview.redacted.txt`: text body preview after truncation and redaction; binary bodies are omitted.

The command does not persist raw cookies, raw auth headers, or raw network logs. URL userinfo and sensitive query parameters are redacted in
stdout, JSON, and artifact files.

## Output contract

Human-readable text is the default. JSON is opt-in with `--format json` on `capture`.

JSON output uses a versioned envelope:

```json
{
  "schema_version": "cli.web-evidence.capture.v1",
  "command": "web-evidence capture",
  "ok": true,
  "result": {
    "artifact_dir": "/tmp/web-evidence",
    "method": "GET",
    "requested_url": "https://example.com/",
    "final_url": "https://example.com/",
    "status_code": 200,
    "status_class": "success",
    "body_bytes_captured": 128,
    "body_truncated": false,
    "artifacts": []
  }
}
```

Failure envelopes use `ok=false` with stable `error.code`, `error.message`, and optional `error.details`. HTTP `4xx` and `5xx` responses
are classified as `http-status-error` and still keep the redacted artifact bundle.

Exit codes:

- `0`: success
- `1`: runtime failure, network failure, or HTTP status failure
- `64`: usage/configuration error

## Docs

- [Docs index](docs/README.md)
- [CLI service JSON contract guideline](../../docs/specs/cli-service-json-contract-guideline-v1.md)
- [New CLI crate development standard](../../docs/runbooks/new-cli-crate-development-standard.md)
