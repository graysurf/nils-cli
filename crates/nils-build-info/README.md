# nils-build-info

Build-time metadata helper for nils CLI binaries.

The crate exposes compile-time constants for `git describe --tags --always
--dirty` and `rustc --version`, plus a `long_version` helper used by clap root
commands. It has no runtime dependencies.

## Docs

- [`docs/README.md`](docs/README.md)
