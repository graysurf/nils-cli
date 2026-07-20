# agent-session Docs

Crate-local documentation for the `agent-session` CLI.

- [CLI README and operator guide](../README.md)
- [Session coordination v1 contract](specs/session-coordination-v1.md)
- [Activity stream v1 contract](specs/activity-stream-v1.md)
- [Session maintenance v1 contract](specs/session-maintenance-v1.md)
- [Completion migration contract](reports/agent-session-completion-migration-contract.md)

This index and the CLI README define CLI, operator, and protocol contracts. For
global agent-facing policy and collision response, use agent-runtime-kit's
canonical
[`session-coordination.md`](https://github.com/graysurf/agent-runtime-kit/blob/main/core/policies/session-coordination.md).
Work context and presence provide collision awareness; they do not grant user
or repository authorization. Default `advisory` and unmanaged sessions never
require a claim, while explicit `enforce` retains the strict claim and admission
protocol defined by the v1 contract.
