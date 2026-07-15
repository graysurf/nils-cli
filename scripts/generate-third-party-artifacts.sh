#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  generate-third-party-artifacts.sh --write
  generate-third-party-artifacts.sh --check

Generates deterministic third-party dependency artifacts from:
  cargo metadata --format-version 1 --locked

Artifacts:
  - THIRD_PARTY_LICENSES.md
  - THIRD_PARTY_NOTICES.md

Options:
  --write   Regenerate both artifacts in-place.
  --check   Verify both artifacts are up-to-date; exits non-zero on drift.
  -h, --help
USAGE
}

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 2
fi

mode="${1:-}"
case "$mode" in
  --write|--check) ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    echo "error: unknown argument: $mode" >&2
    usage >&2
    exit 2
    ;;
esac

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

metadata_json="$tmp_dir/cargo-metadata.json"
licenses_tmp="$tmp_dir/THIRD_PARTY_LICENSES.md"
notices_tmp="$tmp_dir/THIRD_PARTY_NOTICES.md"

cargo metadata --format-version 1 --locked --manifest-path "$repo_root/Cargo.toml" > "$metadata_json"

python3 - "$repo_root" "$metadata_json" "$licenses_tmp" "$notices_tmp" <<'PY'
import collections
import hashlib
import json
import pathlib
import re
import sys
from urllib.parse import urlparse


def md_cell(value: str) -> str:
    return value.replace("\\", "\\\\").replace("|", "\\|").replace("\n", " ").strip()


def cargo_source_label(source: str) -> str:
    if source == "registry+https://github.com/rust-lang/crates.io-index":
        return "crates.io"
    if source.startswith("registry+"):
        return source.removeprefix("registry+")
    if source.startswith("git+"):
        trimmed = source.removeprefix("git+").split("#", 1)[0]
        return trimmed
    return source


def cargo_source_url(pkg: dict) -> str | None:
    source = (pkg.get("source") or "").strip()
    name = (pkg.get("name") or "").strip()
    version = (pkg.get("version") or "").strip()

    if source == "registry+https://github.com/rust-lang/crates.io-index" and name and version:
        return f"https://crates.io/crates/{name}/{version}"
    if source.startswith("registry+"):
        return source.removeprefix("registry+")
    if source.startswith("git+"):
        return source.removeprefix("git+").split("#", 1)[0]
    return None


def normalize_manifest_dir(manifest_path: str) -> pathlib.Path:
    parsed = urlparse(manifest_path)
    if parsed.scheme == "file":
        return pathlib.Path(parsed.path).resolve().parent
    return pathlib.Path(manifest_path).resolve().parent


def crate_top_level_files(crate_dir: pathlib.Path) -> list[str]:
    if not crate_dir.is_dir():
        return []
    files = [child.name for child in crate_dir.iterdir() if child.is_file()]
    files.sort(key=lambda value: value.lower())
    return files


def casefold_name_lookup(file_names: list[str]) -> dict[str, str]:
    lookup: dict[str, str] = {}
    for name in file_names:
        key = name.lower()
        if key not in lookup:
            lookup[key] = name
    return lookup


def find_notice_files(crate_dir: pathlib.Path) -> list[str]:
    preferred = [
        "NOTICE",
        "NOTICE.md",
        "NOTICE.txt",
        "NOTICE.rst",
        "notice",
        "notice.md",
        "notice.txt",
        "notice.rst",
    ]
    file_names = crate_top_level_files(crate_dir)
    name_lookup = casefold_name_lookup(file_names)

    found: list[str] = []
    seen: set[str] = set()
    for candidate in preferred:
        key = candidate.lower()
        resolved_name = name_lookup.get(key)
        if resolved_name and key not in seen:
            found.append(resolved_name)
            seen.add(key)

    dynamic = []
    for file_name in file_names:
        if re.match(r"(?i)^notice(?:[._-].*)?$", file_name):
            key = file_name.lower()
            if key not in seen:
                dynamic.append(file_name)
                seen.add(key)
    found.extend(dynamic)

    return found


def resolve_license_file_ref(crate_dir: pathlib.Path, raw_ref: str | None) -> str | None:
    if not raw_ref:
        return None

    ref = raw_ref.strip()
    if not ref:
        return None

    candidate = pathlib.Path(ref)
    if candidate.is_absolute():
        target = candidate
    else:
        target = crate_dir / candidate

    if target.exists():
        try:
            return str(target.relative_to(crate_dir))
        except ValueError:
            return str(target)
    return ref


def find_license_files(crate_dir: pathlib.Path) -> list[str]:
    preferred = [
        "LICENSE",
        "LICENSE.md",
        "LICENSE.txt",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "COPYING",
        "COPYING.md",
        "COPYING.txt",
        "UNLICENSE",
        "UNLICENSE.txt",
    ]
    file_names = crate_top_level_files(crate_dir)
    name_lookup = casefold_name_lookup(file_names)

    found: list[str] = []
    seen: set[str] = set()
    for candidate in preferred:
        key = candidate.lower()
        resolved_name = name_lookup.get(key)
        if resolved_name and key not in seen:
            found.append(resolved_name)
            seen.add(key)

    dynamic = []
    for file_name in file_names:
        if re.match(r"(?i)^(license|copying|unlicense)(?:[._-].*)?$", file_name):
            key = file_name.lower()
            if key not in seen:
                dynamic.append(file_name)
                seen.add(key)
    found.extend(dynamic)

    return found


repo_root = pathlib.Path(sys.argv[1]).resolve()
metadata_path = pathlib.Path(sys.argv[2]).resolve()
licenses_path = pathlib.Path(sys.argv[3]).resolve()
notices_path = pathlib.Path(sys.argv[4]).resolve()

with metadata_path.open("r", encoding="utf-8") as fh:
    metadata = json.load(fh)

all_packages = metadata.get("packages", [])
third_party = [pkg for pkg in all_packages if pkg.get("source")]
workspace_packages = [pkg for pkg in all_packages if not pkg.get("source")]

third_party.sort(
    key=lambda pkg: (
        pkg.get("name", ""),
        pkg.get("version", ""),
        pkg.get("source", ""),
        pkg.get("id", ""),
    )
)

lockfile_path = repo_root / "Cargo.lock"
lock_hash = hashlib.sha256(lockfile_path.read_bytes()).hexdigest()
peekaboo_lock_path = repo_root / "crates" / "macos-agent" / "peekaboo-lock.json"
peekaboo_lock = json.loads(peekaboo_lock_path.read_text(encoding="utf-8"))
peekaboo_tag = peekaboo_lock["tag"]
peekaboo_repo = peekaboo_lock["repository"]
peekaboo_license = peekaboo_lock["license"]


def license_value(pkg: dict) -> str:
    license_expr = (pkg.get("license") or "").strip()
    if license_expr:
        return license_expr
    license_file = (pkg.get("license_file") or "").strip()
    if license_file:
        return f"SEE LICENSE FILE ({license_file})"
    return "UNKNOWN"


license_counter: collections.Counter[str] = collections.Counter()
for pkg in third_party:
    license_counter[license_value(pkg)] += 1

license_summary_rows = sorted(license_counter.items(), key=lambda entry: (-entry[1], entry[0]))

license_lines: list[str] = [
    "# THIRD_PARTY_LICENSES",
    "",
    "This file documents third-party Rust crate licenses used by this workspace.",
    "",
    "- Data source: `cargo metadata --format-version 1 --locked`",
    f"- Cargo.lock SHA256: `{lock_hash}`",
    f"- Third-party crates (`source != null`): {len(third_party)}",
    f"- Workspace crates (`source == null`, excluded below): {len(workspace_packages)}",
    "",
    "## Notes",
    "",
    "- `License` values are taken from each crate's Cargo metadata (`license` or `license_file`).",
    "- `Source` is the resolved package source from Cargo metadata.",
    "- This list is generated from the current `Cargo.lock`; dependency or script changes require regeneration.",
    "",
    "## License Summary",
    "",
    "| License Expression | Crate Count |",
    "| --- | ---: |",
]

for expression, count in license_summary_rows:
    license_lines.append(f"| {md_cell(expression)} | {count} |")

license_lines.extend(
    [
        "",
        "## Dependency List",
        "",
        "| Crate | Version | License | Source |",
        "| --- | --- | --- | --- |",
    ]
)

for pkg in third_party:
    name = pkg.get("name", "")
    version = pkg.get("version", "")
    source = cargo_source_label(pkg.get("source", ""))
    license_expr = license_value(pkg)
    license_lines.append(
        f"| {md_cell(name)} | {md_cell(version)} | {md_cell(license_expr)} | {md_cell(source)} |"
    )

license_lines.extend(
    [
        "",
        "## Locked Runtime Backend",
        "",
        f"- Peekaboo `{peekaboo_tag}` is licensed under `{peekaboo_license['spdx']}`.",
        f"- Source and license: <{peekaboo_license['source']}>",
        "- The nils-cli package does not embed Peekaboo. `macos-agent backend install` downloads the exact official release assets named and SHA256-locked in `crates/macos-agent/peekaboo-lock.json` into user-scoped storage.",
    ]
)

licenses_path.write_text("\n".join(license_lines) + "\n", encoding="utf-8")

fallback_notice_line = "No explicit NOTICE file discovered."

notice_lines: list[str] = [
    "# THIRD_PARTY_NOTICES",
    "",
    "This file documents third-party notice-file discovery for Rust crates used by this workspace.",
    "",
    "- Data source: `cargo metadata --format-version 1 --locked`",
    f"- Cargo.lock SHA256: `{lock_hash}`",
    f"- Third-party crates (`source != null`): {len(third_party)}",
    "",
    "## Notice Extraction Policy",
    "",
    "- The generator checks each crate directory for notice files using deterministic name matching.",
    "- If no notice file is found, the fallback wording below is emitted exactly.",
    f"- Standard fallback wording: `{fallback_notice_line}`",
    "",
    "## Dependency Notices",
    "",
]

for pkg in third_party:
    name = pkg.get("name", "")
    version = pkg.get("version", "")
    source = cargo_source_label(pkg.get("source", ""))
    license_expr = license_value(pkg)
    manifest_path = pkg.get("manifest_path", "")
    crate_dir = normalize_manifest_dir(manifest_path) if manifest_path else pathlib.Path(".")

    notice_refs = find_notice_files(crate_dir)
    license_refs: list[str] = []
    seen_license_refs: set[str] = set()

    metadata_license_ref = resolve_license_file_ref(crate_dir, pkg.get("license_file"))
    if metadata_license_ref:
        license_refs.append(metadata_license_ref)
        seen_license_refs.add(metadata_license_ref.lower())

    for discovered_ref in find_license_files(crate_dir):
        key = discovered_ref.lower()
        if key in seen_license_refs:
            continue
        license_refs.append(discovered_ref)
        seen_license_refs.add(key)

    notice_lines.append(f"### {name} {version}")
    notice_lines.append("")
    notice_lines.append(f"- License: `{license_expr}`")
    notice_lines.append(f"- Source: `{source}`")
    is_mpl = re.search(r"(?i)\bMPL-2\.0\b", license_expr) is not None
    source_url = cargo_source_url(pkg)
    if source_url and is_mpl:
        notice_lines.append(f"- Source URL: <{source_url}>")
    if is_mpl:
        notice_lines.append("- License text (MPL-2.0): <https://mozilla.org/MPL/2.0/>")
    if notice_refs:
        notice_lines.append("- Notice files:")
        for ref in notice_refs:
            notice_lines.append(f"  - `{ref}`")
    else:
        notice_lines.append(f"- Notice files: {fallback_notice_line}")

    if license_refs:
        notice_lines.append("- License file references:")
        for ref in license_refs:
            notice_lines.append(f"  - `{ref}`")
    else:
        notice_lines.append("- License file reference: none declared")

    notice_lines.append("")

notice_lines.extend(
    [
        "## Locked Runtime Backend Notice",
        "",
        f"### Peekaboo {peekaboo_tag}",
        "",
        f"- License: `{peekaboo_license['spdx']}`",
        f"- Source: <{peekaboo_repo}/tree/{peekaboo_tag}>",
        f"- License text: <{peekaboo_license['source']}>",
        "- Distribution model: official release assets are downloaded on demand, verified against the immutable lock, and are not embedded in nils-cli release artifacts.",
        "",
    ]
)

notices_path.write_text("\n".join(notice_lines), encoding="utf-8")
PY

artifacts=("THIRD_PARTY_LICENSES.md" "THIRD_PARTY_NOTICES.md")
generated=("$licenses_tmp" "$notices_tmp")

case "$mode" in
  --write)
    for idx in "${!artifacts[@]}"; do
      cp "${generated[$idx]}" "${artifacts[$idx]}"
    done
    echo "PASS: regenerated ${artifacts[*]}"
    ;;
  --check)
    drift=0
    for idx in "${!artifacts[@]}"; do
      artifact="${artifacts[$idx]}"
      candidate="${generated[$idx]}"
      if [[ ! -f "$artifact" ]]; then
        echo "FAIL: missing required artifact: $artifact" >&2
        drift=1
        continue
      fi
      if ! cmp -s "$artifact" "$candidate"; then
        echo "FAIL: artifact drift detected: $artifact" >&2
        diff -u "$artifact" "$candidate" || true
        drift=1
      fi
    done

    if [[ "$drift" -ne 0 ]]; then
      echo "FAIL: third-party artifacts are stale; run: bash scripts/generate-third-party-artifacts.sh --write" >&2
      exit 1
    fi

    echo "PASS: third-party artifacts are up-to-date"
    ;;
esac
