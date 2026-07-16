#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: detect-release-only.sh --base <git-ref> [--head <git-ref>] --branch <name>

Prints `true` only for a single canonical nils-cli release bump commit whose
branch matches the target version. Every unverifiable or non-canonical change
prints `false` so callers can fall back to full CI.
USAGE
}

base_ref=""
head_ref="HEAD"
branch=""
base_seen=0
branch_seen=0
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --base)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      base_ref="$2"
      base_seen=1
      shift 2
      ;;
    --head)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      head_ref="$2"
      shift 2
      ;;
    --branch)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      branch="$2"
      branch_seen=1
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: ${1:-}" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$base_seen" -ne 1 || "$branch_seen" -ne 1 ]]; then
  usage >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]] || ! command -v python3 >/dev/null 2>&1; then
  echo false
  exit 0
fi

verdict="$({ python3 - "$repo_root" "$base_ref" "$head_ref" "$branch" <<'PY'
from __future__ import annotations

import hashlib
import re
import subprocess
import sys
from pathlib import Path

repo = Path(sys.argv[1])
base_input, head_input, branch = sys.argv[2:]


def git(*args: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(repo), *args],
        stderr=subprocess.DEVNULL,
        text=True,
    )


def git_show(ref: str, path: str) -> str:
    return git("show", f"{ref}:{path}")


def tree_paths(ref: str) -> list[str]:
    return git("ls-tree", "-r", "--name-only", ref).splitlines()


def tree_mode(ref: str, path: str) -> str:
    output = git("ls-tree", ref, "--", path).strip()
    return output.split(maxsplit=1)[0] if output else "missing"


def workspace_version(text: str) -> str:
    match = re.search(
        r'(?ms)^\[workspace[.]package\].*?^version\s*=\s*"([^"]+)"', text
    )
    if not match:
        raise ValueError("workspace version is missing")
    return match.group(1)


def stable_version(value: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", value)
    if not match:
        raise ValueError("version is not stable semver")
    return tuple(int(part) for part in match.groups())


def package_name(text: str) -> str | None:
    section = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped.strip("[]")
            continue
        if section == "package":
            found = re.match(r'\s*name\s*=\s*"([^"]+)"\s*$', line)
            if found:
                return found.group(1)
    return None


def classify() -> bool:
    if not base_input or not head_input or not branch:
        return False
    base_ref = git("rev-parse", "--verify", f"{base_input}^{{commit}}").strip()
    head_ref = git("rev-parse", "--verify", f"{head_input}^{{commit}}").strip()
    parents = git("rev-list", "--parents", "-n", "1", head_ref).split()
    if len(parents) != 2 or parents[1] != base_ref:
        return False
    if git("rev-list", "--count", f"{base_ref}..{head_ref}").strip() != "1":
        return False

    base_root = git_show(base_ref, "Cargo.toml")
    head_root = git_show(head_ref, "Cargo.toml")
    old_version = workspace_version(base_root)
    version = workspace_version(head_root)
    if stable_version(version) <= stable_version(old_version):
        return False
    if branch != f"chore/release-{version.replace('.', '-')}":
        return False
    if git("show", "-s", "--format=%s", head_ref).rstrip("\n") != (
        f"chore(release): bump cli versions to {version}"
    ):
        return False

    base_paths = tree_paths(base_ref)
    manifest_paths = ["Cargo.toml"] + sorted(
        path
        for path in base_paths
        if re.fullmatch(r"crates/[^/]+/Cargo[.]toml", path)
    )
    head_manifest_paths = ["Cargo.toml"] + sorted(
        path
        for path in tree_paths(head_ref)
        if re.fullmatch(r"crates/[^/]+/Cargo[.]toml", path)
    )
    if manifest_paths != head_manifest_paths:
        return False

    managed_paths = set(manifest_paths) | {
        "Cargo.lock",
        "README.md",
        "THIRD_PARTY_LICENSES.md",
        "THIRD_PARTY_NOTICES.md",
    }
    for path in managed_paths:
        if tree_mode(base_ref, path) != tree_mode(head_ref, path):
            return False

    changed = git("diff", "--name-only", base_ref, head_ref).splitlines()
    if not changed or any(path not in managed_paths for path in changed):
        return False
    raw_changes = git("diff", "--raw", "--no-renames", base_ref, head_ref).splitlines()
    if len(raw_changes) != len(changed):
        return False
    for row in raw_changes:
        metadata, _, _path = row.partition("\t")
        fields = metadata.split()
        if len(fields) != 5 or fields[4] != "M" or fields[0][1:] != fields[1]:
            return False

    base_manifests = {path: git_show(base_ref, path) for path in manifest_paths}
    workspace_packages = {
        name for text in base_manifests.values() if (name := package_name(text))
    }
    expected_changed: set[str] = set()

    for path, text in base_manifests.items():
        section = None
        output: list[str] = []
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("[") and stripped.endswith("]"):
                section = stripped.strip("[]")
            if section in {"package", "workspace.package"}:
                found = re.match(r'(\s*version\s*=\s*)"[^"]+"(.*)', line)
                if found:
                    line = f'{found.group(1)}"{version}"{found.group(2)}'
            dependency = re.match(
                r'(\s*([A-Za-z0-9_.-]+)\s*=\s*\{)(.*)(\}\s*(?:#.*)?)$', line
            )
            if dependency:
                key = dependency.group(2).strip('"')
                body = dependency.group(3)
                package = re.search(r'\bpackage\s*=\s*"([^"]+)"', body)
                package = package.group(1) if package else key
                if package in workspace_packages and re.search(r"\bpath\s*=", body):
                    if re.search(r"\bversion\s*=", body):
                        body = re.sub(
                            r'(\bversion\s*=\s*)"[^"]+"',
                            rf'\1"{version}"',
                            body,
                            count=1,
                        )
                    else:
                        index = re.search(r"\bpath\s*=", body).start()
                        body = body[:index] + f'version = "{version}", ' + body[index:]
                    line = f"{dependency.group(1)}{body}{dependency.group(4)}"
            output.append(line)
        expected = "\n".join(output) + ("\n" if text.endswith("\n") else "")
        if expected != text:
            expected_changed.add(path)
        if git_show(head_ref, path) != expected:
            return False

    base_lock = git_show(base_ref, "Cargo.lock")
    head_lock = git_show(head_ref, "Cargo.lock")
    lines = base_lock.splitlines(keepends=True)
    starts = [index for index, line in enumerate(lines) if line.strip() == "[[package]]"]
    starts.append(len(lines))
    for position in range(len(starts) - 1):
        start, end = starts[position], starts[position + 1]
        block = lines[start:end]
        name = None
        has_source = False
        for line in block:
            found = re.match(r'\s*name\s*=\s*"([^"]+)"\s*$', line.rstrip("\r\n"))
            if found:
                name = found.group(1)
            if re.match(r"\s*source\s*=", line):
                has_source = True
        if name in workspace_packages and not has_source:
            changed_version = False
            for index in range(start, end):
                found = re.match(
                    rf'(\s*version\s*=\s*)"{re.escape(old_version)}"(\s*(?:\r?\n)?)$',
                    lines[index],
                )
                if found:
                    lines[index] = f'{found.group(1)}"{version}"{found.group(2)}'
                    changed_version = True
                    break
            if not changed_version:
                return False

    in_dependencies = False
    for index, line in enumerate(lines):
        stripped = line.strip()
        if re.match(r"dependencies\s*=\s*\[", stripped):
            in_dependencies = True
            continue
        if in_dependencies and stripped == "]":
            in_dependencies = False
            continue
        if not in_dependencies:
            continue
        found = re.match(
            r'(\s*")([^" ]+) ([^" ]+)([^\"]*)("[^\r\n]*)(\r?\n)?$', line
        )
        if not found:
            continue
        name, dependency_version, suffix = found.group(2), found.group(3), found.group(4)
        if (
            name in workspace_packages
            and dependency_version == old_version
            and not suffix.strip()
        ):
            lines[index] = (
                f"{found.group(1)}{name} {version}{suffix}{found.group(5)}"
                f"{found.group(6) or ''}"
            )
    expected_lock = "".join(lines)
    if expected_lock != base_lock:
        expected_changed.add("Cargo.lock")
    if head_lock != expected_lock:
        return False

    base_lock_hash = hashlib.sha256(base_lock.encode()).hexdigest()
    head_lock_hash = hashlib.sha256(head_lock.encode()).hexdigest()
    for path in ("THIRD_PARTY_LICENSES.md", "THIRD_PARTY_NOTICES.md"):
        base_text = git_show(base_ref, path)
        expected = base_text.replace(base_lock_hash, head_lock_hash)
        if expected != base_text:
            expected_changed.add(path)
        if git_show(head_ref, path) != expected:
            return False

    base_readme = git_show(base_ref, "README.md")
    output = []
    patterns = ("tag like `v", "git tag -a v", "git push origin v")
    for line in base_readme.splitlines():
        if any(pattern in line for pattern in patterns):
            line = re.sub(r"v\d+\.\d+\.\d+", f"v{version}", line)
        output.append(line)
    expected_readme = "\n".join(output) + ("\n" if base_readme.endswith("\n") else "")
    if expected_readme != base_readme:
        expected_changed.add("README.md")
    if git_show(head_ref, "README.md") != expected_readme:
        return False

    return set(changed) == expected_changed


try:
    print("true" if classify() else "false")
except (OSError, subprocess.CalledProcessError, UnicodeError, ValueError):
    print("false")
PY
} 2>/dev/null)" || verdict=false

case "$verdict" in
  true|false) echo "$verdict" ;;
  *) echo false ;;
esac
