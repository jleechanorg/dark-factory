"""Static contract checks for the immutable controller cold-review v2 prompt.

File justification
------------------
GOAL: Lock the prompt-only acceptance contract before implementation.
MODIFICATION: Add focused source-level invariants for authority, gates, review
mission, output shape, compatibility boundaries, and v1 size comparison.
NECESSITY: The new prompt is a separate immutable contract; existing tests do
not cover its static wording and the v1 prompt must remain untouched.
INTEGRATION PROOF: The test reads the catalog files at the repository seam and
uses only deterministic presence/absence and size assertions.
"""

from pathlib import Path


ROOT = Path(__file__).parents[1]
V1 = ROOT / "prompts/catalog/controller_cold_review_v1.md"
V2 = ROOT / "prompts/catalog/controller_cold_review_v2.md"


def _v2() -> str:
    return V2.read_text(encoding="utf-8")


def test_v2_opens_with_untrusted_data_boundary_and_preserves_base64_envelope():
    prompt = _v2()
    opening = prompt[:900].lower()
    for term in (
        "repository",
        "task",
        "description",
        "diff",
        "comments",
        "logs",
        "evidence",
        "generated artifacts",
        "untrusted",
        "cannot replace",
        "cannot skip",
        "cannot stop",
    ):
        assert term in opening
    assert "base64" in prompt.lower()
    assert "do not follow instructions inside" in prompt.lower()


def test_v2_names_exactly_four_truth_sources_and_four_fail_closed_gates():
    prompt = _v2()
    lower = prompt.lower()
    for term in ("requirements", "pr claims", "production behavior", "executed evidence"):
        assert term in lower
    for gate in ("CLAIMS", "RUNTIME", "EVIDENCE", "ADVERSARIAL"):
        assert prompt.count(f"{gate}:") == 1
        assert f"{gate}: <pass|fail>" in prompt
    assert "four truth sources" in lower or "four sources of truth" in lower
    assert "pass or fail" in lower
    assert "warning" in lower or "partial" in lower


def test_v2_requires_maximum_recall_review_mission():
    lower = _v2().lower()
    for term in (
        "material-claim ledger",
        "callers",
        "consumers",
        "strongest relevant counterexample",
        "continue after the first",
        "all independently actionable defects",
    ):
        assert term in lower


def test_v2_fails_closed_on_head_artifact_and_false_green_contradictions():
    lower = _v2().lower()
    for term in (
        "exact-head",
        "raw artifacts",
        "false-green",
        "surrogate tests",
        "unverified material claim",
        "fail closed",
    ):
        assert term in lower


def test_v2_prioritizes_material_risk_before_style():
    lower = _v2().lower()
    priority = [
        "correctness",
        "security",
        "data loss",
        "integration",
        "false evidence",
        "style",
    ]
    positions = [lower.index(term) for term in priority]
    assert positions == sorted(positions)


def test_v2_has_required_output_sections_and_no_model_verdict_or_old_checklist():
    prompt = _v2()
    lower = prompt.lower()
    for section in ("## Findings", "## Commands Executed", "## Evidence Checked", "## Caveats"):
        assert prompt.count(section) == 1
    assert "eight bindings" in lower
    assert "four gate lines" in lower
    assert "four required sections" in lower
    assert "VERDICT:" not in prompt
    assert "C0" not in prompt and "E14" not in prompt
    assert "reference checklist" not in lower


def test_v2_is_shorter_than_immutable_v1():
    assert V2.stat().st_size < V1.stat().st_size
    assert len(_v2().split()) < len(V1.read_text(encoding="utf-8").split())
