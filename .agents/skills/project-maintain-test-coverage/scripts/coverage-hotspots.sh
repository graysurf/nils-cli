#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: coverage-hotspots.sh [--lcov PATH] [--limit N] [--all]

Ranks file-level LCOV entries with uncovered lines. By default, only files
under crates/ are included.

Options:
  --lcov PATH   LCOV input path (default: target/coverage/lcov.info)
  --limit N     Maximum rows to print (default: 20)
  --all         Include non-crates/ files
  -h, --help    Show this help
USAGE
}

lcov_path="target/coverage/lcov.info"
limit="20"
include_all=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --lcov)
      [[ $# -ge 2 ]] || {
        echo "error: --lcov requires a path" >&2
        exit 2
      }
      lcov_path="$2"
      shift 2
      ;;
    --limit)
      [[ $# -ge 2 ]] || {
        echo "error: --limit requires a number" >&2
        exit 2
      }
      limit="$2"
      shift 2
      ;;
    --all)
      include_all=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "error: unexpected argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$limit" in
  "" | *[!0-9]*)
    echo "error: --limit must be a positive integer" >&2
    exit 2
    ;;
esac

if [[ "$limit" -eq 0 ]]; then
  echo "error: --limit must be greater than zero" >&2
  exit 2
fi

if [[ ! -f "$lcov_path" ]]; then
  echo "error: missing LCOV file: $lcov_path" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$script_dir" rev-parse --show-toplevel)"

rows="$(
  awk -F: -v repo_root="$repo_root" -v include_all="$include_all" '
    function normalize(path) {
      gsub(/\\/, "/", path)
      if (index(path, repo_root "/") == 1) {
        path = substr(path, length(repo_root) + 2)
      }
      return path
    }

    function emit() {
      if (sf == "" || lf <= 0) {
        return
      }
      file = normalize(sf)
      if (!include_all && file !~ /^crates\//) {
        return
      }
      missing = lf - lh
      if (missing <= 0) {
        return
      }
      pct = (lh / lf) * 100
      printf "%.6f\t%d\t%s\t%d\t%d\n", pct, missing, file, lh, lf
    }

    $1 == "SF" {
      emit()
      sf = substr($0, 4)
      lf = 0
      lh = 0
      next
    }
    $1 == "LF" {
      lf = $2 + 0
      next
    }
    $1 == "LH" {
      lh = $2 + 0
      next
    }
    END {
      emit()
    }
  ' "$lcov_path" \
    | sort -t "$(printf '\t')" -k1,1n -k2,2nr \
    | awk -v limit="$limit" 'NR <= limit { print }'
)"

cat <<EOF
# Coverage Hotspots

Source: \`$lcov_path\`

EOF

if [[ -z "$rows" ]]; then
  echo "No uncovered lines found for the selected scope."
  exit 0
fi

cat <<'EOF'
| file | line coverage | hit | found | missing |
| --- | ---: | ---: | ---: | ---: |
EOF

while IFS=$'\t' read -r pct missing file lh lf; do
  pct_text="$(awk -v pct="$pct" 'BEGIN { printf "%.2f%%", pct }')"
  printf "| \`%s\` | %s | %d | %d | %d |\n" "$file" "$pct_text" "$lh" "$lf" "$missing"
done <<<"$rows"
