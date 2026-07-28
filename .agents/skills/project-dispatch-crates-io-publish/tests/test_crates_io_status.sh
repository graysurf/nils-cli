#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$script_dir" rev-parse --show-toplevel)"
entrypoint="${repo_root}/scripts/crates-io-status.sh"

fail() {
  echo "error: $*" >&2
  exit 1
}

assert_contains() {
  local file="$1"
  local pattern="$2"
  if ! rg -q "$pattern" "$file"; then
    echo "error: expected pattern '$pattern' in $file" >&2
    sed -n '1,220p' "$file" >&2 || true
    exit 1
  fi
}

create_mock_cargo() {
  local dir="$1"
  cat > "${dir}/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != "metadata" ]]; then
  echo "unexpected cargo command: $*" >&2
  exit 1
fi

cat <<'JSON'
{
  "packages": [
    {
      "name": "nils-a",
      "version": "1.2.3",
      "publish": null,
      "dependencies": []
    },
    {
      "name": "nils-b",
      "version": "1.2.4",
      "publish": null,
      "dependencies": []
    }
  ]
}
JSON
MOCK
  chmod +x "${dir}/cargo"
}

create_mock_api_server_script() {
  local path="$1"
  cat > "$path" <<'PY'
#!/usr/bin/env python3
from __future__ import annotations

import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

fixture_path, port_file = sys.argv[1], sys.argv[2]
with open(fixture_path, "r", encoding="utf-8") as fp:
    fixtures = json.load(fp)


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        entry = fixtures.get(self.path)
        if entry is None:
            status = 404
            payload = {"errors": [{"detail": "not found"}]}
        else:
            status = int(entry.get("status", 200))
            payload = entry.get("body", {})
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _fmt: str, *_args: object) -> None:
        return


server = HTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w", encoding="utf-8") as fp:
    fp.write(str(server.server_port))
server.serve_forever()
PY
  chmod +x "$path"
}

start_mock_api() {
  local server_py="$1"
  local fixture="$2"
  local port_file="$3"
  local log_file="$4"
  local pid_var="$5"
  python3 "$server_py" "$fixture" "$port_file" >"$log_file" 2>&1 &
  local server_pid=$!
  for _ in $(seq 1 80); do
    if [[ -s "$port_file" ]]; then
      break
    fi
    sleep 0.05
  done
  if [[ ! -s "$port_file" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    fail "mock api did not start"
  fi
  printf -v "$pid_var" '%s' "$server_pid"
}

cleanup_mock_api_test_case() {
  local status=$?
  local cleanup_status=0
  trap - EXIT

  if [[ -n "${pid:-}" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if kill -0 "$pid" 2>/dev/null; then
      echo "error: mock api survived cleanup: $pid" >&2
      cleanup_status=1
    fi
  fi

  if [[ -n "${tmp:-}" ]]; then
    rm -rf -- "$tmp"
    if [[ -e "$tmp" ]]; then
      echo "error: test temp directory survived cleanup: $tmp" >&2
      cleanup_status=1
    fi
  fi

  if [[ "$status" -ne 0 ]]; then
    exit "$status"
  fi
  exit "$cleanup_status"
}

test_explicit_version_fail_on_missing() (
  local tmp pid=""
  tmp="$(mktemp -d)"
  trap cleanup_mock_api_test_case EXIT
  local bin_dir="${tmp}/bin"
  mkdir -p "$bin_dir"
  create_mock_cargo "$bin_dir"

  local fixture="${tmp}/fixture.json"
  cat > "$fixture" <<'JSON'
{
  "/api/v1/crates/nils-a": {"status": 200, "body": {"crate": {"name": "nils-a", "newest_version": "1.2.3", "updated_at": "2026-02-11T10:00:00Z"}}},
  "/api/v1/crates/nils-b": {"status": 200, "body": {"crate": {"name": "nils-b", "newest_version": "1.2.4", "updated_at": "2026-02-11T10:00:00Z"}}},
  "/api/v1/crates/nils-a/1.2.3": {"status": 200, "body": {"version": {"num": "1.2.3", "created_at": "2026-02-11T10:01:00Z", "yanked": false, "downloads": 5}}},
  "/api/v1/crates/nils-b/1.2.3": {"status": 404, "body": {"errors": [{"detail": "missing"}]}}
}
JSON

  local server_py="${tmp}/mock_api.py"
  local port_file="${tmp}/port.txt"
  local api_log="${tmp}/api.log"
  create_mock_api_server_script "$server_py"
  start_mock_api "$server_py" "$fixture" "$port_file" "$api_log" pid
  local port
  port="$(cat "$port_file")"

  local json_out="${tmp}/status.json"
  set +e
  CRATES_IO_STATUS_CARGO_BIN="${bin_dir}/cargo" \
    CRATES_IO_STATUS_API_BASE="http://127.0.0.1:${port}/api/v1" \
    "$entrypoint" --crates "nils-a nils-b" --version v1.2.3 --format json --json-out "$json_out" --fail-on-missing \
    >"${tmp}/stdout.log" 2>"${tmp}/stderr.log"
  local rc=$?
  set -e

  [[ "$rc" -eq 1 ]] || fail "expected exit code 1, got $rc"
  python3 - "$json_out" <<'PY'
from __future__ import annotations
import json
import sys

data = json.load(open(sys.argv[1], "r", encoding="utf-8"))
assert data["query"]["mode"] == "explicit-version"
assert data["query"]["target_version"] == "1.2.3"
by_name = {item["crate"]: item for item in data["results"]}
assert by_name["nils-a"]["status"] == "published"
assert by_name["nils-b"]["status"] == "missing"
assert data["summary"]["missing"] == 1
print("ok")
PY
)

test_workspace_mode_text_and_json() (
  local tmp pid=""
  tmp="$(mktemp -d)"
  trap cleanup_mock_api_test_case EXIT
  local bin_dir="${tmp}/bin"
  mkdir -p "$bin_dir"
  create_mock_cargo "$bin_dir"

  local fixture="${tmp}/fixture.json"
  cat > "$fixture" <<'JSON'
{
  "/api/v1/crates/nils-a": {"status": 200, "body": {"crate": {"name": "nils-a", "newest_version": "1.2.3", "updated_at": "2026-02-11T10:00:00Z"}}},
  "/api/v1/crates/nils-b": {"status": 200, "body": {"crate": {"name": "nils-b", "newest_version": "1.2.4", "updated_at": "2026-02-11T10:00:00Z"}}},
  "/api/v1/crates/nils-a/1.2.3": {"status": 200, "body": {"version": {"num": "1.2.3", "created_at": "2026-02-11T10:01:00Z", "yanked": false, "downloads": 5}}},
  "/api/v1/crates/nils-b/1.2.4": {"status": 200, "body": {"version": {"num": "1.2.4", "created_at": "2026-02-11T10:02:00Z", "yanked": false, "downloads": 8}}}
}
JSON

  local server_py="${tmp}/mock_api.py"
  local port_file="${tmp}/port.txt"
  local api_log="${tmp}/api.log"
  create_mock_api_server_script "$server_py"
  start_mock_api "$server_py" "$fixture" "$port_file" "$api_log" pid
  local port
  port="$(cat "$port_file")"

  local json_out="${tmp}/status.json"
  local text_out="${tmp}/status.md"
  CRATES_IO_STATUS_CARGO_BIN="${bin_dir}/cargo" \
    CRATES_IO_STATUS_API_BASE="http://127.0.0.1:${port}/api/v1" \
    "$entrypoint" --crates "nils-a nils-b" --format both --json-out "$json_out" --text-out "$text_out" \
    >"${tmp}/stdout.log" 2>"${tmp}/stderr.log"

  assert_contains "$text_out" "# crates.io Status Report"
  assert_contains "$text_out" "\\| nils-a \\| 1.2.3 \\| 1.2.3 \\| published \\|"
  assert_contains "$text_out" "\\| nils-b \\| 1.2.4 \\| 1.2.4 \\| published \\|"
  python3 - "$json_out" <<'PY'
from __future__ import annotations
import json
import sys

data = json.load(open(sys.argv[1], "r", encoding="utf-8"))
assert data["query"]["mode"] == "workspace-version"
assert data["summary"]["published"] == 2
assert data["summary"]["missing"] == 0
print("ok")
PY
)

test_mock_api_cleanup_failure_path() (
  local probe_root="$1"
  local tmp="${probe_root}/case"
  local pid=""
  mkdir -p "$tmp"
  trap cleanup_mock_api_test_case EXIT

  local fixture="${tmp}/fixture.json"
  local server_py="${tmp}/mock_api.py"
  local port_file="${tmp}/port.txt"
  local api_log="${tmp}/api.log"
  printf '{}\n' >"$fixture"
  create_mock_api_server_script "$server_py"
  start_mock_api "$server_py" "$fixture" "$port_file" "$api_log" pid
  printf '%s\n' "$pid" >"${probe_root}/pid.txt"

  return 23
)

if [[ ! -x "$entrypoint" ]]; then
  fail "missing executable: $entrypoint"
fi

test_explicit_version_fail_on_missing
test_workspace_mode_text_and_json

cleanup_probe_root="$(mktemp -d)"
trap 'rm -rf -- "$cleanup_probe_root"' EXIT
set +e
test_mock_api_cleanup_failure_path "$cleanup_probe_root"
cleanup_probe_rc=$?
set -e
[[ "$cleanup_probe_rc" -eq 23 ]] || fail "expected cleanup probe exit code 23, got $cleanup_probe_rc"
cleanup_probe_pid="$(cat "${cleanup_probe_root}/pid.txt")"
if kill -0 "$cleanup_probe_pid" 2>/dev/null; then
  fail "mock api survived a failing test case"
fi
[[ ! -e "${cleanup_probe_root}/case" ]] || fail "failing test case temp directory survived cleanup"
rm -rf -- "$cleanup_probe_root"
trap - EXIT

echo "ok: crates.io status tests passed"
