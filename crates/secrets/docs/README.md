# secrets docs

`secrets` owns pull/add of a repo's `.env` from a central SOPS store. It is a
thin orchestrator over `sops` and `git`; it never parses or renders the
*contents* of an encrypted store entry.

See the crate [README](../README.md) for the command surface, output modes,
exit codes, and the no-secret-leak guarantee. The shared JSON envelope contract
lives in `docs/specs/cli-service-json-contract-guideline-v1.md`.
