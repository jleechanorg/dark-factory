#!/usr/bin/env bash
set -euo pipefail

BASE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_FILE="${BASE_DIR}/spec_review/fullstack_smoke_report.json"

REQUIRED_FILES=(
  "$BASE_DIR/backend/main.py"
  "$BASE_DIR/frontend/index.html"
  "$BASE_DIR/firestore.rules"
  "$BASE_DIR/scripts/validate_spec.py"
)

for path in "${REQUIRED_FILES[@]}"; do
  if [ ! -f "$path" ]; then
    echo "FULLSTACK_SMOKE_MISSING_FILE: $path"
    echo "{\"verdict\":\"fail\",\"reason\":\"missing required artifact\"}" > "$REPORT_FILE"
    exit 1
  fi
done

python - "$BASE_DIR" "$REPORT_FILE" <<'PY'
import json
from pathlib import Path
import sys

base = Path(sys.argv[1]).resolve()
report_file = Path(sys.argv[2]).resolve()
checks = {}

def fail(reason: str) -> int:
    payload = {
        "verdict": "fail",
        "reason": reason,
        "checks": checks,
    }
    report_file.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    raise SystemExit(1)

backend = (base / "backend" / "main.py").read_text(encoding="utf-8", errors="replace")
frontend = (base / "frontend" / "index.html").read_text(encoding="utf-8", errors="replace")
firestore = (base / "firestore.rules").read_text(encoding="utf-8", errors="replace")

checks["backend/main.py"] = {
    "size_bytes": len(backend.encode("utf-8")),
    "contains_app_or_main": bool("def app" in backend or "if __name__" in backend),
}
checks["frontend/index.html"] = {
    "size_bytes": len(frontend.encode("utf-8")),
    "contains_html_root": bool("<html" in frontend.lower()),
}
checks["firestore.rules"] = {
    "size_bytes": len(firestore.encode("utf-8")),
    "contains_rules_version": "rules_version" in firestore,
    "contains_service_cloud": "service cloud.firestore" in firestore,
}

if checks["backend/main.py"]["size_bytes"] <= 0:
    fail("backend/main.py is empty")
if checks["frontend/index.html"]["size_bytes"] <= 0 or not checks["frontend/index.html"]["contains_html_root"]:
    fail("frontend/index.html invalid")
if checks["firestore.rules"]["size_bytes"] <= 0 or not checks["firestore.rules"]["contains_rules_version"]:
    fail("firestore.rules invalid")

report_file.write_text(
    json.dumps(
        {
            "verdict": "pass",
            "checks": checks,
        },
        indent=2,
    ),
    encoding="utf-8",
)
print("FULLSTACK_SMOKE_OK")
PY

# Optional runtime check for a local emulator endpoint.
if [ -n "${FIRESTORE_EMULATOR_HOST:-}" ]; then
  echo "Checking FIRESTORE_EMULATOR_HOST=$FIRESTORE_EMULATOR_HOST"
  python - <<'PY'
import os
import socket
import urllib.request

host = os.environ.get("FIRESTORE_EMULATOR_HOST", "")
if not host:
    raise SystemExit(0)

host_only = host.split("/")[0]
parts = host_only.split(":")
addr = parts[0]
port = int(parts[1]) if len(parts) > 1 else 8080

with socket.create_connection((addr, port), timeout=3):
    pass
with urllib.request.urlopen(f"http://{host_only}", timeout=3) as response:
    response.read(64)
PY
fi

exit 0
