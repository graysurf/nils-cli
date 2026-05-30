"""Shared changed-path classification for the nils-cli CI / local-fast lanes.

Single source of truth for two questions:

- ``is_doc_path(path)``: is this changed path documentation (so it can take the
  docs-only lane)?
- ``affects_third_party_artifacts(path)``: does this path feed the generated
  third-party license/notice artifacts (so it must escape the docs-only lane)?

Both ``scripts/ci/nils-cli-local-fast.sh`` (the changed-scope planner) and
``scripts/ci/detect-docs-only.sh`` (the CI docs-only gate) import these helpers.
Keeping one definition prevents the markdownlint vs. local-fast divergence that
let an MD060 violation reach a release tag.
"""


def is_doc_path(path):
    # Inside a crate, only the crate README and the crate docs/ tree are
    # documentation. Every other .md under a crate (include_str! templates and
    # snapshots, plan-template.md, Markdown test fixtures / golden files) is a
    # source or test asset whose contents are asserted by that crate's tests, so
    # it must NOT take the docs-only lane. This mirrors the markdownlint-audit.sh
    # scope and docs/specs/crate-docs-placement-policy.md, which exclude embedded
    # template assets and test fixtures under non-docs directories.
    if path.startswith("crates/"):
        rel = path.split("/", 2)[2] if path.count("/") >= 2 else ""
        return rel == "README.md" or rel.startswith("docs/")
    return (
        path.endswith(".md")
        or path.startswith("docs/")
        or "/docs/" in path
        or path in {"README", "LICENSE", "NOTICE"}
    )


def affects_third_party_artifacts(path):
    return (
        path in {
            "Cargo.toml",
            "Cargo.lock",
            "THIRD_PARTY_LICENSES.md",
            "THIRD_PARTY_NOTICES.md",
            "scripts/generate-third-party-artifacts.sh",
            "scripts/ci/third-party-artifacts-audit.sh",
        }
        or (path.startswith("crates/") and path.endswith("/Cargo.toml"))
    )
