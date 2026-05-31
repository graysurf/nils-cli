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
  python3 "$server_py" "$fixture" "$port_file" >"$log_file" 2>&1 &
  local pid=$!
  for _ in $(seq 1 80); do
    if [[ -s "$port_file" ]]; then
      break
    fi
    sleep 0.05
  done
  [[ -s "$port_file" ]] || fail "mock api did not start"
  echo "$pid"
}

test_explicit_version_fail_on_missing() {
  local tmp
  tmp="$(mktemp -d)"
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
  local pid
  pid="$(start_mock_api "$server_py" "$fixture" "$port_file" "$api_log")"
  trap "kill $pid 2>/dev/null || true" RETURN
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
  trap - RETURN
}

test_workspace_mode_text_and_json() {
  local tmp
  tmp="$(mktemp -d)"
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
  local pid
  pid="$(start_mock_api "$server_py" "$fixture" "$port_file" "$api_log")"
  trap "kill $pid 2>/dev/null || true" RETURN
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
  trap - RETURN
}

if [[ ! -x "$entrypoint" ]]; then
  fail "missing executable: $entrypoint"
fi

test_explicit_version_fail_on_missing
test_workspace_mode_text_and_json

echo "ok: crates.io status tests passed"
