#!/usr/bin/env bash
# Independent reviewer launcher. Invoked from a tool node.
# Usage:
#   review_with_codex.sh <workdir> <spec_path> <output_json> [reviewer_prompt]

set -euo pipefail

WORKDIR="${1:?workdir required}"
SPEC_PATH="${2:?spec path required}"
OUTPUT_JSON="${3:?output json path required}"
PROMPT_TEMPLATE="${4:-$WORKDIR/benchmarks/attractor-spec-review/prompts/reviewer.md}"

if [ ! -f "$SPEC_PATH" ]; then
    echo "ERROR: spec file not found: $SPEC_PATH" >&2
    exit 1
fi
if [ ! -f "$PROMPT_TEMPLATE" ]; then
    echo "ERROR: reviewer prompt not found: $PROMPT_TEMPLATE" >&2
    exit 1
fi

CODEX_BIN="${CODEX_BIN:-codex}"
mkdir -p "$(dirname "$OUTPUT_JSON")"

PROMPT="$(python - "$WORKDIR" "$SPEC_PATH" "$PROMPT_TEMPLATE" <<'PY'
from __future__ import annotations

from pathlib import Path
import sys

workdir = Path(sys.argv[1]).resolve()
spec_path = Path(sys.argv[2]).resolve()
prompt_path = Path(sys.argv[3]).resolve()

spec_lines = spec_path.read_text(encoding="utf-8").splitlines()
numbered_spec = "\n".join(f"{idx + 1:04d}: {line}" for idx, line in enumerate(spec_lines))

snapshot_paths = [
    "benchmarks/attractor-spec-review/README.md",
    "spec/feature.md",
    "spec/visible_acceptance.md",
    "scripts/validate_spec.py",
    "scripts/fullstack_smoke.sh",
    "backend/main.py",
    "frontend/index.html",
    "firestore.rules",
]

existing = []
for rel in snapshot_paths:
    p = workdir / rel
    status = "present" if p.exists() else "missing"
    existing.append(f"{rel} ({status})")

candidate_snapshot = "\n".join(f"- {item}" for item in existing)
validation_report = workdir / "spec_review" / "validation_report.json"

template = prompt_path.read_text(encoding="utf-8")
template = template.replace("${spec_path}", str(spec_path))
template = template.replace("${validation_report}", str(validation_report))
template = template.replace("${candidate_snapshot}", candidate_snapshot)
template = template.replace("${spec_lines}", numbered_spec or "(empty spec)")
print(template)
PY
)"

RAW_OUTPUT="$(mktemp)"
INPUT_FILE="$(mktemp)"
trap 'rm -f "$RAW_OUTPUT" "$INPUT_FILE"' EXIT
printf "%s\n" "$PROMPT" > "$INPUT_FILE"

if ! command -v "$CODEX_BIN" >/dev/null 2>&1; then
    echo "ERROR: codex executable not found: $CODEX_BIN" >&2
    exit 1
fi

set +e
"$CODEX_BIN" exec --yolo --skip-git-repo-check - < "$INPUT_FILE" > "$RAW_OUTPUT" 2>&1
CODEX_RC=$?
set -e

python - "$WORKDIR" "$SPEC_PATH" "$OUTPUT_JSON" "$CODEX_RC" "$RAW_OUTPUT" <<'PY'
from __future__ import annotations

import json
from json.decoder import JSONDecoder, JSONDecodeError
import re
import sys
from pathlib import Path

workdir = Path(sys.argv[1]).resolve()
spec_path = Path(sys.argv[2]).resolve()
output_json = Path(sys.argv[3]).resolve()
codex_rc = int(sys.argv[4])
raw_output_path = Path(sys.argv[5]).resolve()

raw_text = raw_output_path.read_text(encoding="utf-8", errors="replace")


def parse_last_json(text: str):
    decoder = JSONDecoder()
    candidates = []
    for match in re.finditer(r"\{", text):
        idx = match.start()
        try:
            obj, _ = decoder.raw_decode(text[idx:])
        except JSONDecodeError:
            continue
        else:
            if isinstance(obj, dict):
                candidates.append(obj)
    return candidates[-1] if candidates else None


payload = parse_last_json(raw_text)
spec_lines = spec_path.read_text(encoding="utf-8").splitlines()
reviewable_spec_lines = [idx for idx, line in enumerate(spec_lines, start=1) if line.strip()]
required_finding_keys = {"line", "severity", "issue", "evidence", "fix_hint"}

errors = []
normalized = {
    "verdict": "fail",
    "source": {
        "workdir": str(workdir),
        "spec_path": str(spec_path),
        "raw_output_path": str(raw_output_path),
        "codex_returncode": codex_rc,
    },
    "validation": {"total_lines": len(spec_lines)},
    "coverage": {
        "total_lines": len(spec_lines),
        "reviewed_lines": [],
        "missing_lines": list(range(1, len(spec_lines) + 1)),
    },
    "findings": [],
    "summary": {
        "critical_gap_count": 0,
        "blocking_gap_count": 0,
        "overall_recommendation": "Run failed before review output could be parsed.",
    },
    "parse_error": None,
    "raw_output": raw_text[-4000:],
}

if codex_rc != 0:
    errors.append(f"codex exited with rc={codex_rc}")

if payload is None:
    errors.append("No JSON object found in codex output.")
else:
    normalized["coverage"] = payload.get("coverage", normalized["coverage"])
    normalized["findings"] = payload.get("findings", [])
    normalized["summary"] = payload.get("summary", normalized["summary"])
    normalized["spec"] = payload.get("spec", {})
    normalized["verdict"] = ("pass" if str(payload.get("verdict", "fail")).lower() == "pass" else "fail")
    normalized["validation"] = payload.get("validation", normalized["validation"])

    if not isinstance(normalized["coverage"], dict):
        errors.append("coverage must be an object.")
    else:
        reviewed = normalized["coverage"].get("reviewed_lines")
        missing = normalized["coverage"].get("missing_lines")
        if not isinstance(reviewed, list) or not isinstance(missing, list):
            errors.append("coverage.reviewed_lines and coverage.missing_lines must be arrays.")
        elif normalized["verdict"] == "pass":
            reviewed_set = {int(item) for item in reviewed if isinstance(item, int) or str(item).isdigit()}
            missing_reviewable = sorted(set(reviewable_spec_lines) - reviewed_set)
            if missing_reviewable:
                preview = ", ".join(str(item) for item in missing_reviewable[:20])
                errors.append(f"reviewer pass did not cover all non-empty spec lines: {preview}")
            if missing:
                preview = ", ".join(str(item) for item in missing[:20])
                errors.append(f"reviewer pass reported missing lines: {preview}")

    if not isinstance(normalized["findings"], list):
        errors.append("findings must be an array.")
    else:
        for item in normalized["findings"]:
            if not isinstance(item, dict):
                errors.append("each finding must be an object.")
                break
            missing_keys = required_finding_keys.difference(item.keys())
            if missing_keys:
                errors.append("finding missing keys: " + ", ".join(sorted(missing_keys)))
                break

    if normalized["verdict"] == "pass" and errors:
        normalized["verdict"] = "fail"

if errors:
    normalized["parse_error"] = "; ".join(errors)

if normalized["verdict"] == "pass" and codex_rc != 0:
    normalized["verdict"] = "fail"

output_code = 0 if normalized["verdict"] == "pass" else 1
output_json.write_text(json.dumps(normalized, indent=2), encoding="utf-8")
print(f"Reviewer verdict written to: {output_json}")
print(f"Reviewer verdict: {normalized['verdict']}")
sys.exit(output_code)
PY
