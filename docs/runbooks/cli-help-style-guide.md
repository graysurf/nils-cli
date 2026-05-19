# CLI Help Style Guide

## Purpose

Every user-facing workspace CLI should make common usage, configuration,
environment variables, and exit behavior discoverable from root `--help`.
Help text should answer concrete operator questions without becoming a product
overview.

## Root Command Shape

Root help should include:

- A short `about`: one sentence that identifies the binary and its main job.
- A `long_about`: one short paragraph for the durable contract and default
  behavior.
- Usage and commands/options from clap or the binary's custom dispatcher.
- `EXAMPLES`: at least one shell invocation for the primary workflow.
- `ENVIRONMENT`: every environment variable the binary reads or writes.
- `EXIT CODES`: the stable exit-code table for the binary.

When a binary uses a custom help renderer instead of clap's generated help,
preserve the same section names and ordering.

## Clap Attributes

Use clap derive metadata at the root parser when the binary uses clap:

```rust
#[derive(Parser)]
#[command(
    name = "example-cli",
    version,
    about = "Do one durable job.",
    long_about = "Do one durable job with deterministic inputs and structured output.",
    after_help = "EXAMPLES:\n  example-cli run --input input.json\n\nENVIRONMENT:\n  EXAMPLE_HOME  Optional runtime directory.\n\nEXIT CODES:\n  0   success\n  64  command-line usage error\n  1   runtime error"
)]
struct Cli;
```

For environment-backed flags, prefer `#[arg(env = "...")]` only when the
environment variable is already the flag's value source and the precedence is
intended. For binary-wide knobs, cache locations, or deeply nested runtime
toggles, list the variable in `ENVIRONMENT` instead of changing parse-time
behavior.

For mutually exclusive flags, declare the conflict in clap:

```rust
#[arg(long, conflicts_with = "format")]
json: bool,
```

The shared exit-code contract is defined in
`docs/specs/cli-output-contract-v1.md`; help text should summarize the codes
used by the binary rather than redefining that spec.

## Subcommands

Subcommands should have clear one-line help and, for complex flows, a
`long_about` paragraph. Root help does not need to duplicate every subcommand
flag, but it should point users to the right subcommand help.

## Structural Help Tests

Help tests should lock structure, not prose. Assert section headings, expected
commands/options, known environment variable names, and compatibility aliases.
Do not snapshot the full help output line by line.

Recommended assertions:

- root `--help` exits successfully
- output contains `Usage:`
- output contains `EXAMPLES:`
- output contains `ENVIRONMENT:`
- output contains `EXIT CODES:`
- output contains every environment variable the binary reads
- output contains `-V` or `--version` for user-facing clap binaries
