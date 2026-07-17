#!/usr/bin/env python3
"""One-off backfill for jleechan-3wh0: external_ref lineage gap.

Root cause (file:line-cited in the PR description): every in-process
bead-creation call site (intake::normalize / intake::normalize_labeled_prs,
via Tracker::create_bead in daemon/src/tools.rs) has ALWAYS required a
non-optional external_ref. The orphan beads instead trace to tick.rs's
"manual bead adoption" branch (the block that logs INTAKE_BEAD_CREATED with
{"manual": true}), which adopts beads created directly via `br create`
*outside* the daemon (bypassing --external-ref entirely) into local tracking
state. That's a legitimate, sanctioned category (`Bead.external_ref:
Option<String>` is documented "None = manual bead" in tools.rs) -- this
script recovers lineage for the subset of those manual beads whose own
title text unambiguously states which GitHub PR they were about, without
fabricating a link for anything ambiguous.

Usage:
    .venv/bin/python3 daemon/scripts/backfill_external_ref.py [--db PATH] [--dry-run]

Safe to re-run: every mapped bead is checked against the live `br list`
snapshot immediately before update, and any bead that already carries a
non-empty external_ref is skipped (idempotent).
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys

# bead_id -> (external_ref, evidence)
#
# All refs below are grounded in an explicit "PR #<N>" (or "on PR #<N>")
# string that already appears in the orphan bead's own title -- never
# inferred from a branch name or guessed. Where the canonical
# "https://github.com/<owner>/<repo>/pull/<N>" ref for that PR is already
# claimed by a different (earlier-created) bead, we use the "#local-<id>"
# disambiguation suffix -- a convention already present in this exact beads
# store (see jleechan-w1y, jleechan-r69, jleechan-qcs) -- rather than
# violating `br`'s external_ref uniqueness constraint or silently dropping
# the duplicate-target bead's lineage.
BACKFILL_MAP: dict[str, tuple[str, str]] = {
    "jleechan-4dgx": (
        "https://github.com/jleechanorg/worldarchitect.ai/pull/8116#local-4dgx",
        "title: '[worldai] Fix 7 Green Gate workflow failures on PR #8116 "
        "(.github/workflows/)'; canonical pull/8116 ref already held by "
        "jleechan-lio, so disambiguated with #local- suffix (jleechan-w1y "
        "precedent).",
    ),
    "jleechan-6iw7": (
        "https://github.com/jleechanorg/worldarchitect.ai/pull/8116#local-6iw7",
        "title: '[worldai] Fix 7 Green Gate failures on PR #8116 to unblock "
        "/af drive to /green'; duplicate target of jleechan-4dgx, same "
        "disambiguation.",
    ),
    "jleechan-xmdy": (
        "https://github.com/jleechanorg/worldarchitect.ai/pull/8061#local-xmdy",
        "title: '[worldai] Fix 3 Green Gate failures on PR #8061 to unblock "
        "/af drive'; canonical pull/8061 already held by jleechan-s15.",
    ),
    "jleechan-bvaj": (
        "https://github.com/jleechanorg/worldarchitect.ai/pull/8064#local-bvaj",
        "title: '[worldai] Fix 3 Green Gate failures on PR #8064 to unblock "
        "/af drive'; canonical pull/8064 already held by jleechan-qef.",
    ),
    "jleechan-4uzw": (
        "https://github.com/jleechanorg/worldarchitect.ai/pull/8060#local-4uzw",
        "title: '[worldai] Rebase PR #8060 to resolve merge conflicts "
        "(dirty -> MERGEABLE)'; canonical pull/8060 already held by "
        "jleechan-eo8.",
    ),
    "jleechan-hslx": (
        "https://github.com/jleechanorg/worldarchitect.ai/pull/8058#local-hslx",
        "title: '[worldai] Wait for 20 pending CI checks on PR #8058 + fix "
        "any failures'; canonical pull/8058 already held by jleechan-jpi.",
    ),
    "jleechan-93ft": (
        "jleechanorg/worldarchitect.ai#7888",
        "title: '[worldai] Drive PR #7888 (cc-finish-level-commit) to "
        "/green -- rebase + verifier pass'; no other bead held this ref "
        "(created earlier of the two #7888-referencing orphans), so it "
        "gets the canonical form matching the batch-adoption convention.",
    ),
    "jleechan-8dyu": (
        # jleechan-mdgr root-cause fix: the ORIGINAL value here was
        # "jleechanorg/worldarchitect.ai#7888#local-8dyu" -- appending the
        # "#local-<id>" disambiguation suffix onto the SHORT canonical
        # "owner/repo#N" base (which already contains a "#") instead of
        # the "#"-free full-URL base every other duplicate-target entry in
        # this map uses. That produced a ref with 2 "#" characters (3
        # `parse_external_ref`-split segments), which
        # `Tracker::comment_external` (daemon/src/adapters.rs) cannot
        # parse -- this was live-fired against bead jleechan-8dyu on
        # 2026-07-11T00:05:15Z as ESCALATION_NOTIFICATION_FAILED
        # ("parse: invalid external_ref format for comment:
        # jleechanorg/worldarchitect.ai#7888#local-8dyu"). This script
        # already ran (this bead is no longer an orphan, so re-running is
        # a no-op per the `before_orphans` guard below) -- corrected here
        # so the value on record matches the convention actually used, and
        # so no future reuse of this pattern repeats the mistake. The
        # `_assert_no_double_hash_ambiguity` check below now fails loudly
        # if this class of error is reintroduced.
        "https://github.com/jleechanorg/worldarchitect.ai/pull/7888#local-8dyu",
        "title: '[daemon/verify] Post-merge smoke test for "
        "factory-lite-harness-restore PR + first real dispatch on PR "
        "#7888'; duplicate target of jleechan-93ft (created later), "
        "disambiguated.",
    ),
}


def _assert_no_double_hash_ambiguity(backfill_map: dict[str, tuple[str, str]]) -> None:
    """jleechan-mdgr guard: fail loud, before any `br update` call, if any
    mapped ref would double-append a `#local-<id>` disambiguation suffix
    onto a base that already contains its own `#` (e.g. the SHORT canonical
    `owner/repo#N` form). `parse_external_ref` in daemon/src/adapters.rs
    only accepts exactly one `#`, so any entry with 2+ `#` characters is
    guaranteed to break `comment_external` for that bead's escalation
    comments -- catching that here, at data-authoring time, is cheaper than
    an incident.
    """
    bad = {bead_id: ref for bead_id, (ref, _) in backfill_map.items() if ref.count("#") > 1}
    if bad:
        raise ValueError(
            "backfill_external_ref: refusing to run -- these BACKFILL_MAP "
            f"entries have more than one '#' (double-suffix corruption risk): {bad}"
        )


def run_br(args: list[str], db: str | None) -> subprocess.CompletedProcess:
    cmd = ["br"]
    if db:
        cmd += ["--db", db]
    cmd += args
    return subprocess.run(cmd, capture_output=True, text=True, check=False)


def fetch_open_factory_beads(db: str | None) -> list[dict]:
    p = run_br(["list", "--status", "open", "--label", "factory", "--json", "--limit", "0"], db)
    if p.returncode != 0:
        print(f"br list failed (rc={p.returncode}): {p.stderr}", file=sys.stderr)
        sys.exit(1)
    data = json.loads(p.stdout)
    if data.get("has_more"):
        print("br list output truncated (has_more=true); refusing partial view", file=sys.stderr)
        sys.exit(1)
    return data.get("issues", [])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", default=None, help="beads.db path (default: auto-discover)")
    ap.add_argument("--dry-run", action="store_true", help="report only, do not call br update")
    args = ap.parse_args()

    # jleechan-mdgr: fail loud before any br update if the map itself is
    # shaped to corrupt an external_ref (see _assert_no_double_hash_ambiguity).
    _assert_no_double_hash_ambiguity(BACKFILL_MAP)

    before = fetch_open_factory_beads(args.db)
    before_orphans = {i["id"] for i in before if not i.get("external_ref")}
    print(f"BEFORE: {len(before)} open factory beads, {len(before_orphans)} orphans (no external_ref)")

    applied: list[str] = []
    skipped_already_set: list[str] = []
    for bead_id, (ref, evidence) in BACKFILL_MAP.items():
        if bead_id not in before_orphans:
            skipped_already_set.append(bead_id)
            print(f"SKIP  {bead_id}: not an orphan (already has external_ref or not open/factory)")
            continue
        print(f"BACKFILL {bead_id} -> {ref}")
        print(f"    evidence: {evidence}")
        if args.dry_run:
            continue
        p = run_br(["update", bead_id, "--external-ref", ref, "--actor", "jleechan-3wh0-backfill"], args.db)
        if p.returncode != 0:
            print(f"    FAILED (rc={p.returncode}): {p.stderr.strip()}", file=sys.stderr)
            continue
        applied.append(bead_id)

    remaining_unrecoverable = sorted(before_orphans - set(BACKFILL_MAP))
    print(f"\nUNRECOVERABLE ({len(remaining_unrecoverable)}): no explicit PR/issue number in title, "
          "or ambiguous target -- left untouched rather than fabricated:")
    for bead_id in remaining_unrecoverable:
        title = next((i.get("title", "") for i in before if i["id"] == bead_id), "")
        print(f"  {bead_id}: {title}")

    if not args.dry_run:
        after = fetch_open_factory_beads(args.db)
        after_orphans = {i["id"] for i in after if not i.get("external_ref")}
        print(f"\nAFTER: {len(after)} open factory beads, {len(after_orphans)} orphans (no external_ref)")
        print(f"Applied {len(applied)}/{len(BACKFILL_MAP)} backfills: {applied}")
    else:
        print("\n(dry-run: no br update calls were made)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
