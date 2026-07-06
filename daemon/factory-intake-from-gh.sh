#!/usr/bin/env bash
# Deterministic intake: GitHub issues labeled `factory` -> br beads + harness QUEUED.
# Mirrors daemon/src/intake.rs idempotency via external_ref = owner/repo#issue_number.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BR_DB="${BR_DB:-$ROOT/.beads/beads.db}"
br() { command br --db "$BR_DB" "$@"; }
H="${H:-$ROOT/daemon/factory-overlay.sh}"
CONFIG="${CONFIG:-$ROOT/config/daemon.toml}"
FACTORY_LABEL="${FACTORY_LABEL:-factory}"

die() { echo "factory-intake-from-gh: $*" >&2; exit 1; }

USE_HARNESS=1
[ -f "$H" ] || die "overlay harness not found: $H"
[ -f "$CONFIG" ] || CONFIG="$ROOT/daemon/contracts/daemon.toml.example"

TARGET_REPO="$(python3 - "$CONFIG" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
m = re.search(r'^\s*target_repo\s*=\s*"([^"]+)"', text, re.M)
print(m.group(1) if m else "jleechanorg/worldarchitect.ai")
PY
)"

if [ "$USE_HARNESS" = "1" ]; then "$H" init >/dev/null; fi

python3 - "$TARGET_REPO" "$FACTORY_LABEL" "$H" "$USE_HARNESS" <<'PY'
import json, re, subprocess, sys, os
BR_DB = os.environ.get("BR_DB", "")

def br_cmd(args):
    cmd = ["br"]
    if BR_DB:
        cmd += ["--db", BR_DB]
    return cmd + args


target_repo = sys.argv[1]
factory_label = sys.argv[2]
harness = sys.argv[3]
use_harness = sys.argv[4] == "1"

def run(cmd, *, check=True):
    p = subprocess.run(cmd, capture_output=True, text=True)
    if check and p.returncode != 0:
        raise SystemExit(
            f"cmd failed ({p.returncode}): {' '.join(cmd)}\n{p.stderr.strip()}"
        )
    return p

def gh_issues():
    p = run(
        [
            "gh",
            "issue",
            "list",
            "--repo",
            target_repo,
            "--label",
            factory_label,
            "--state",
            "open",
            "--json",
            "number,title,body",
            "--limit",
            "50",
        ]
    )
    return json.loads(p.stdout or "[]")

def br_issues():
    # Fetch BOTH open and closed beads so external_ref duplicate-detection can see
    # already-closed beads that hold a PR's external_ref (closed-after-merge).
    # Without this, intake re-tries to create a new bead for the same issue and
    # `br create` errors with "External reference already exists".
    issues = []
    for status in ("open", "closed"):
        p = run(br_cmd(["list", "--status", status, "--json"]))
        data = json.loads(p.stdout or "{}")
        issues.extend(data.get("issues") or [])
    return issues

def bead_by_external_ref(ref, issues):
    for issue in issues:
        if issue.get("external_ref") == ref:
            return issue.get("id")
    return None

def bead_from_body(body, issues):
    m = re.search(r"(?mi)^\s*[-*]?\s*Bead:\s*(\S+)", body or "")
    if not m:
        return None
    bead_id = m.group(1).strip()
    for issue in issues:
        if issue.get("id") == bead_id:
            return bead_id
    return None

def parse_pr_number(title, body):
    for text in (title or "", body or ""):
        m = re.search(r"PR\s*#(\d+)", text, re.I)
        if m:
            return int(m.group(1))
        m = re.search(r"github\.com/[^/]+/[^/]+/pull/(\d+)", text, re.I)
        if m:
            return int(m.group(1))
    return None

def pr_head_branch(pr_number):
    p = run(
        [
            "gh",
            "pr",
            "view",
            str(pr_number),
            "--repo",
            target_repo,
            "--json",
            "headRefName,state",
        ],
        check=False,
    )
    if p.returncode != 0:
        return None
    data = json.loads(p.stdout or "{}")
    if data.get("state") != "OPEN":
        return None
    return data.get("headRefName")

def ensure_drive_fields(body, pr_number, branch):
    lines = (body or "").splitlines()
    fields = {
        "existing_pr": str(pr_number),
        "existing_branch": branch,
        "target_repo": target_repo,
    }
    out = list(lines)
    present = {k: False for k in fields}
    for i, line in enumerate(out):
        for key in fields:
            if re.match(rf"(?i)^\s*##\s*{re.escape(key)}\s*:", line):
                out[i] = f"## {key}: {fields[key]}"
                present[key] = True
    missing = [k for k, ok in present.items() if not ok]
    if missing:
        if out and out[-1].strip():
            out.append("")
        out.append("## Drive-existing-pr fields (intake normalizer)")
        for key in missing:
            out.append(f"## {key}: {fields[key]}")
    return "\n".join(out)

def intake_upsert(bead_id, title):
    if not use_harness:
        return "skipped_no_harness"
    p = run([harness, "intake-upsert", bead_id, title])
    return p.stdout.strip()

def link_external_ref(bead_id, external_ref):
    run(br_cmd(["update", bead_id, "--external-ref", external_ref]))

issues_gh = gh_issues()
issues_br = br_issues()
created = linked = upserted = skipped = 0

for gh in issues_gh:
    number = gh["number"]
    title = gh.get("title") or f"factory issue #{number}"
    body = gh.get("body") or ""
    external_ref = f"{target_repo}#{number}"

    bead_id = bead_by_external_ref(external_ref, issues_br)
    if not bead_id:
        bead_id = bead_from_body(body, issues_br)

    pr_number = parse_pr_number(title, body)
    drive_mode = bool(pr_number)

    if bead_id:
        existing_ref = next(
            (i.get("external_ref") for i in issues_br if i.get("id") == bead_id),
            None,
        )
        if existing_ref != external_ref:
            link_external_ref(bead_id, external_ref)
            linked += 1
            for i in issues_br:
                if i.get("id") == bead_id:
                    i["external_ref"] = external_ref
    else:
        labels = ["factory"]
        if drive_mode:
            labels.append("drive-existing-pr")
            branch = pr_head_branch(pr_number)
            if branch:
                body = ensure_drive_fields(body, pr_number, branch)
        p = run(br_cmd([
                "create",
                title,
                "--description",
                body,
                "--labels",
                ",".join(labels),
                "--external-ref",
                external_ref,
                "--silent",
            ]))
        bead_id = p.stdout.strip()
        created += 1
        issues_br.append({"id": bead_id, "external_ref": external_ref})

    result = intake_upsert(bead_id, title)
    if result == "created":
        upserted += 1
    elif result == "exists":
        skipped += 1
    print(f"issue #{number} -> {bead_id} ({result}) ext={external_ref}")

print(
    json.dumps(
        {
            "target_repo": target_repo,
            "factory_label": factory_label,
            "gh_issues": len(issues_gh),
            "beads_created": created,
            "external_ref_linked": linked,
            "overlay_created": upserted,
            "overlay_exists": skipped,
        }
    )
)
PY
