#!/usr/bin/env bash
# Public foundation gate for the Amazon Firestore/auth slice.

set -euo pipefail

WORKDIR="${1:-.}"
cd "$WORKDIR"

PORT="${PORT:-31339}"
BASE_URL="http://127.0.0.1:${PORT}"

fail() {
    echo "FOUNDATION_FAIL: $*" >&2
    exit 1
}

require_file() {
    [ -f "$1" ] || fail "missing required file: $1"
}

require_make_target() {
    awk -F: -v target="$1" '$1 == target { found=1 } END { exit found ? 0 : 1 }' Makefile \
        || fail "missing Makefile target: $1"
}

require_file Makefile
require_file package.json
require_file firebase.json
require_file firestore.rules
require_file src/server.js
require_file src/public/index.html
require_file src/public/app.js
require_file src/public/styles.css

for target in build seed run test validate-size; do
    require_make_target "$target"
done

echo "== foundation static checks =="
python - <<'PY'
from pathlib import Path
import json

package = json.loads(Path("package.json").read_text())
deps = {**package.get("dependencies", {}), **package.get("devDependencies", {})}
dep_names = " ".join(sorted(deps)).lower()
if "firebase" not in dep_names and "firestore" not in dep_names:
    raise SystemExit("package.json must include Firebase/Firestore dependencies")

source = "\n".join(
    p.read_text(errors="ignore")
    for p in Path(".").glob("**/*")
    if p.is_file()
    and "node_modules" not in p.parts
    and "package-lock.json" not in p.parts
    and "results" not in p.parts
    and p.suffix in {".js", ".json", ".html", ".css", ".rules", ".md"}
)
required = [
    "FIRESTORE_EMULATOR_HOST",
    "metricsSnapshots",
    "sellerProfiles",
    "moderationEvents",
    "notificationPreferences",
    "POST /auth/login",
    "POST /seed/reset",
    "GET /session",
]
missing = [term for term in required if term.lower() not in source.lower()]
if missing:
    raise SystemExit(f"missing foundation terms: {', '.join(missing)}")

total = 0
frontend = 0
for path in Path(".").glob("**/*"):
    if not path.is_file():
        continue
    if "node_modules" in path.parts or "package-lock.json" in path.parts or "results" in path.parts:
        continue
    if path.suffix not in {".js", ".html", ".css", ".rules"}:
        continue
    count = sum(1 for line in path.read_text(errors="ignore").splitlines() if line.strip())
    total += count
    if str(path).startswith("src/public/"):
        frontend += count
if total < 5000:
    raise SystemExit(f"source line floor not met: {total} < 5000")
if frontend < 2000:
    raise SystemExit(f"frontend line floor not met: {frontend} < 2000")
print(json.dumps({"source_lines": total, "frontend_lines": frontend}))
PY

echo "== make build =="
make build

echo "== make seed =="
make seed

echo "== make test =="
make test

echo "== make validate-size =="
make validate-size

echo "== foundation runtime smoke =="
PORT="$PORT" make run > /tmp/amazon-foundation-run.log 2>&1 &
server_pid=$!
cleanup() {
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
}
trap cleanup EXIT

python - "$BASE_URL" <<'PY'
import http.cookiejar
import json
import sys
import time
import urllib.error
import urllib.request

base = sys.argv[1]
jar = http.cookiejar.CookieJar()
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))

def fetch(path, method="GET", payload=None, expected=(200,)):
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with opener.open(req, timeout=3) as resp:
            body = resp.read().decode()
            if resp.status not in expected:
                raise RuntimeError(f"{path} status {resp.status}, expected {expected}")
            return body
    except urllib.error.HTTPError as exc:
        if exc.code in expected:
            return exc.read().decode()
        raise

for _ in range(50):
    try:
        fetch("/health")
        break
    except Exception:
        time.sleep(0.2)
else:
    raise SystemExit("server did not become healthy")

fetch("/health")
fetch("/metrics")
fetch("/api/metrics")
fetch("/seed/reset", method="POST", expected=(200, 201, 204))
fetch("/api/seed/reset", method="POST", expected=(200, 201, 204))

registered = fetch(
    "/auth/register",
    method="POST",
    payload={"email": "foundation@example.com", "password": "password123", "role": "shopper"},
    expected=(200, 201, 409),
)
login_payloads = [
    {"username": "shopper", "password": "password"},
    {"email": "shopper@example.com", "password": "password"},
    {"email": "foundation@example.com", "password": "password123"},
]
logged_in = False
for path in ("/auth/login", "/api/auth/login"):
    for payload in login_payloads:
        try:
            fetch(path, method="POST", payload=payload, expected=(200, 201))
            logged_in = True
            break
        except Exception:
            continue
    if logged_in:
        break
if not logged_in:
    raise SystemExit("login failed on root and api auth endpoints")

session = fetch("/session")
api_session = fetch("/api/session")
diagnostics = fetch("/diagnostics")
api_diagnostics = fetch("/api/diagnostics")
fetch("/auth/logout", method="POST", expected=(200, 204))

print(json.dumps({
    "registered": bool(registered),
    "session": bool(session),
    "api_session": bool(api_session),
    "diagnostics": bool(diagnostics),
    "api_diagnostics": bool(api_diagnostics),
}))
PY

echo "foundation public acceptance passed"
