"""Universal + custom-prompt + slash gate family.

Rationale (jleechan-arr): all three call sites in this module use the
**1200s** default when a node omits the ``timeout`` attribute. The
roadmap at ``docs/plans/factory_improvement_analysis.md`` proposes
**300s** for "review / deep auditing". Observed production gate wall
clocks (gate_es / gate_er / gate_code_standards across the CXDB event
log) show p99 = 206s (max observed = 206s, never timed out). The 1200s
default intentionally leaves headroom for very large diffs that would
exceed the 300s roadmap value; it does not slow down the median case
(the OS-level subprocess timeout only fires after the wall clock
exceeds the bound). See ``TIMEOUT_DEFAULTS_RATIONALE`` in
``docs/plans/factory_improvement_analysis.implementation.md`` Pillar 4
for the empirical distribution and the citation.

Owns:
  * `_slash_gate` — factory for ``/<command>`` reviewer gates with SHA binding.
  * `UNIVERSAL_CODE_STANDARDS_PROMPT` — embedded prompt template.
  * `UNIVERSAL_EVIDENCE_REVIEW_PROMPT` — embedded prompt template.
  * `_run_universal_prompt_gate` — run a gate using an embedded universal
    prompt (no local cmd file).
  * `_node_prompt_ref` — read ``prompt="@path"`` attr.
  * `_run_custom_prompt_gate` — render an authored ``prompt=`` template +
    append the machine contract.
  * `_gate_es` — es.md → slash OR universal fallback OR custom prompt.
  * `_gate_er` — er.md / evidence_review.md → slash OR universal fallback
    OR custom prompt.
  * `_gate_code_standards` — code-standards.md → slash OR universal fallback
    OR custom prompt.
  * `UNIVERSAL_DEAD_CODE_PROMPT` — embedded prompt template (declared here so
    the dead-code gate stays in the same family as the others).
  * `_gate_dead_code` — dead-code.md → slash OR universal fallback OR custom
    prompt.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Callable, Optional

import runner.handlers as _handlers_shim

from .handler_core import Result, _gate_strict_flag

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context, Handler


CODER_HANDOFF_FORMAT = """\
Before the final verdict, include a section titled `## Coder Handoff` with:
- Summary: one or two sentences describing what you actually verified.
- Blocking findings: each blocker with file/path, line or artifact reference, and why it fails.
- Evidence checked: exact commands, logs, screenshots, videos, URLs, or files you inspected.
- Required fix: concrete implementation steps the coder should take next.
- Verification to rerun: exact commands or artifacts that should prove the fix.

If there are no blockers, still include the section and state `Blocking findings: none`.
"""


def _slash_gate(slash_command: str, default_args: str = "") -> "Handler":
    """Build a handler that shells out to the reviewer backend with `/<command> <args>`."""

    def handler(node: "Node", ctx: "Context") -> "Result":
        args = node.attrs.get("args", default_args)
        target = node.attrs.get("target", str(ctx.workdir))

        # Echo backend: outcome from state hint, used by tests + CI. The
        # echo path does not call out to a subprocess so SHA binding is not
        # applicable — tests that exercise SHA binding must use the claude
        # backend with a fake binary on PATH.
        if ctx.backend in ("echo", "mock_llm"):
            hint = ctx.state.get(f"{node.name}.outcome", "success")
            return Result(
                outcome=hint,
                output=f"echo gate /{slash_command}: pre-seeded {hint}",
                metadata={"slash_command": slash_command, "verdict": "echo:" + hint},
            )

        # SHA binding: compute the worktree HEAD and require the gate to
        # echo it back. If we cannot compute a SHA (not a git worktree, git
        # missing), the gate cannot be safely run — return `error`.
        expected_sha = _handlers_shim._worktree_head_sha(ctx.workdir)
        if expected_sha is None:
            return Result(
                outcome="error",
                output=f"gate /{slash_command} cannot resolve HEAD SHA for {ctx.workdir}",
                metadata={
                    "slash_command": slash_command,
                    "verdict": "unknown",
                    "head_sha_status": "missing",
                },
            )

        sha_directive = (
            f"\n\n"
            f"<!-- RUNNER BINDING REQUIREMENT (non-negotiable) -->\n"
            f"expected_head_sha: {expected_sha}\n\n"
            f"CRITICAL: Your response MUST include the following line verbatim "
            f"(machine-parsed binding — do NOT omit it, paraphrase it, "
            f"or put it inside a code block):\n"
            f"head_sha: {expected_sha}\n\n"
            f"Place this line near the top of your response, right after any "
            f"header/context block. The pipeline runner rejects responses missing this line.\n"
        )
        prompt = f"/{slash_command} {args} {target}".strip() + sha_directive
        timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "1200"), 1200)

        # Reviewer backend routing + agy→claude infra fallback live in
        # _execute_gate. The gate is read-only and SHA-bound regardless of
        # which backend grades the diff.
        backend, gate_meta = _handlers_shim._resolve_gate_backend(node, ctx)
        result = _handlers_shim._execute_gate(
            prompt, expected_sha, timeout, ctx, slash_command, backend,
            gate_strict=_gate_strict_flag(node),
        )
        if gate_meta:
            for k, v in gate_meta.items():
                result.metadata.setdefault(k, v)
        return result

    handler.__name__ = f"_gate_{slash_command}"  # noqa: WPS125
    return handler


UNIVERSAL_CODE_STANDARDS_PROMPT = """\
You are performing an automated, repository-agnostic Code Standards & Quality Review.
You have full read-write tool access to the current workspace. Proactively use your tools to inspect, build, run tests, and actively verify the code.

You MUST audit the implementation against the following core principles:

1. ZERO FRAMEWORK COGNITION (ZFC):
   - Avoid dependency/framework bloat. Prioritize lightweight, native logical primitives.
   - Do not pull in heavy abstractions or third-party wrappers unnecessarily.
   - Ensure the solution uses standard library/native features where applicable.

2. ROOT-CAUSE-FIRST ENGINEERING:
   - Verify that the changes directly address the true root cause of the feature request or bug.
   - Look for and reject "workaround" logic, such as generic try/catch suppression, defensive
     fallback values, sanitizers, or clamp layers that merely mask upstream errors.
   - Prohibit fixing symptoms when upstream prompts, schemas, or instructions should be corrected.

3. CLEAN CODE & CODE QUALITY:
   - Readability, proper modularity, clean interface boundaries, and appropriate type annotations.
   - No trailing debugging logs/comments or placeholders.

Provide a detailed review report listing:
- A brief summary of scope.
- Audit results for ZFC, Root-Cause-First, and Clean Code.
- A bulleted list of any blockers and required fixes.

Before the final verdict, include a section titled `## Coder Handoff` with:
- Summary: one or two sentences describing what you actually verified.
- Blocking findings: each blocker with file/path, line or artifact reference, and why it fails.
- Evidence checked: exact commands, logs, screenshots, videos, URLs, or files you inspected.
- Required fix: concrete implementation steps the coder should take next.
- Verification to rerun: exact commands or artifacts that should prove the fix.

If there are no blockers, still include the section and state `Blocking findings: none`.

CRITICAL FORMATTING INSTRUCTIONS:
1. You MUST include a binding verification line:
   head_sha: {expected_sha}

2. You MUST conclude your review with:
   verdict: <pass|warn|fail>
"""


UNIVERSAL_EVIDENCE_REVIEW_PROMPT = """\
You are performing an automated, repository-agnostic Evidence Standards & Review check.
You have full read-write tool access to the current workspace. Proactively use your tools to inspect, read, run, and verify evidence and test execution.

You MUST audit the implementation's evidence against the following core standards:

1. GIT PROVENANCE & STALENESS:
   - Check that metadata records git provenance: HEAD SHA, branch, and merge base.
   - Staleness is about whether the evidence still reflects the code under review,
     NOT exact SHA equality with the current HEAD ({expected_sha}). PASS this standard
     when HEAD is at, or a descendant of, the recorded evidence/fix commit AND the
     production (non-test, non-evidence) files relevant to the claim are unchanged
     between the recorded SHA and HEAD (verify with git, e.g.
     `git merge-base --is-ancestor <recorded_sha> HEAD` plus a diff of the claim's
     production paths). Committed in-repo evidence is EXPECTED to be an ancestor of
     HEAD — a commit cannot embed its own tip SHA — so an evidence/fix commit that sits
     one or more commits behind HEAD does NOT fail this standard as long as the certified
     production code is unchanged. Ignore test-only and evidence-only commits when judging
     staleness. Only fail when provenance is missing entirely, or when the certified
     production behavior actually diverges between the recorded SHA and HEAD.

2. METRICS & TELEMETRY:
   - Execution duration (or equivalent timing) MUST be recorded — FAIL this standard if no
     timing is present at all. Token usage and cost are OPTIONAL / best-effort: when the
     executing backend or harness does not surface them, that is reported as "N/A (not
     emitted by backend)" and MUST NOT fail this standard. PASS only when timing is present
     (whether or not token/cost are available); do not fail solely on missing token or cost
     figures.

3. RESULT INVARIANTS:
   - Ensure all executed scenarios are explicitly marked as passed with no silent failures.
   - An intended RED/failing result that is clearly labeled as the proof state (e.g. a
     bug-reproduction that must fail on vulnerable code) is a documented invariant, not a
     silent failure.

4. CHECKSUM INTEGRITY:
   - Confirm that generated files are accompanied by valid checksums or integrity metadata.

Before the final verdict, include a section titled `## Coder Handoff` with:
- Summary: one or two sentences describing what you actually verified.
- Blocking findings: each blocker with file/path, line or artifact reference, and why it fails.
- Evidence checked: exact commands, logs, screenshots, videos, URLs, or files you inspected.
- Required fix: concrete implementation steps the coder should take next.
- Verification to rerun: exact commands or artifacts that should prove the fix.

If there are no blockers, still include the section and state `Blocking findings: none`.

CRITICAL FORMATTING INSTRUCTIONS:
1. You MUST include a binding verification line:
   head_sha: {expected_sha}

2. You MUST conclude your review with:
   verdict: <pass|fail>
"""


def _run_universal_prompt_gate(
    prompt_template: str, name: str, node: "Node", ctx: "Context"
) -> "Result":
    """Run a gate review using an embedded universal prompt (no local slash command file)."""
    if ctx.backend in ("echo", "mock_llm"):
        hint = ctx.state.get(f"{node.name}.outcome", "success")
        return Result(
            outcome=hint,
            output=f"echo gate {name}: pre-seeded {hint}",
            metadata={"slash_command": name, "verdict": "echo:" + hint},
        )

    expected_sha = _handlers_shim._worktree_head_sha(ctx.workdir)
    if expected_sha is None:
        return Result(
            outcome="error",
            output=f"gate {name} cannot resolve HEAD SHA for {ctx.workdir}",
            metadata={"slash_command": name, "verdict": "unknown", "head_sha_status": "missing"},
        )

    sha_directive = (
        f"\n\n<!-- RUNNER BINDING REQUIREMENT (non-negotiable) -->\n"
        f"expected_head_sha: {expected_sha}\n\n"
        f"CRITICAL: Your response MUST include the following line verbatim:\n"
        f"head_sha: {expected_sha}\n"
    )
    prompt = prompt_template.format(expected_sha=expected_sha) + sha_directive
    timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "1200"), 1200)

    # Reviewer backend routing + agy→claude infra fallback live in
    # _execute_gate (shared with _slash_gate).
    backend, gate_meta = _handlers_shim._resolve_gate_backend(node, ctx)
    result = _handlers_shim._execute_gate(
        prompt, expected_sha, timeout, ctx, name, backend,
        gate_strict=_gate_strict_flag(node),
    )
    if gate_meta:
        for k, v in gate_meta.items():
            result.metadata.setdefault(k, v)
    return result


def _node_prompt_ref(node: "Node") -> Optional[str]:
    """Prompt template ref from node attrs (mirrors ``Node.prompt_ref``).

    Reads ``attrs`` directly so gate routing also works for duck-typed test
    nodes that don't carry the parser property.
    """
    ref = node.attrs.get("prompt")
    if not ref:
        return None
    ref = str(ref)
    return ref[1:] if ref.startswith("@") else ref


def _run_custom_prompt_gate(node: "Node", ctx: "Context", name: str) -> "Result":
    """Run a gate review using the node's own prompt template (``prompt="@path"``).

    A graph that authors review instructions on a gate node owns the review
    *content*; the runner still owns the machine contract — SHA binding and
    the ``verdict: <pass|fail>`` marker are appended here so
    ``_parse_verdict`` / ``_verify_head_sha_echo`` grade a custom gate
    exactly like the slash and universal gates.
    """
    if ctx.backend in ("echo", "mock_llm"):
        hint = ctx.state.get(f"{node.name}.outcome", "success")
        return Result(
            outcome=hint,
            output=f"echo gate {name}: pre-seeded {hint}",
            metadata={"slash_command": name, "verdict": "echo:" + hint},
        )

    expected_sha = _handlers_shim._worktree_head_sha(ctx.workdir)
    if expected_sha is None:
        return Result(
            outcome="error",
            output=f"gate {name} cannot resolve HEAD SHA for {ctx.workdir}",
            metadata={"slash_command": name, "verdict": "unknown", "head_sha_status": "missing"},
        )

    rendered = _handlers_shim._render_prompt(node, ctx)
    # _render_prompt degrades to a goal-only stub when the template is
    # missing or outside the allowed roots. A coder node can limp along on
    # the stub; a review gate must not silently grade with no instructions.
    ref = _node_prompt_ref(node)
    if f"(missing prompt: {ref})" in rendered or f"(invalid prompt: {ref})" in rendered:
        return Result(
            outcome="error",
            output=f"gate {name}: prompt template unavailable: {ref}",
            metadata={"slash_command": name, "verdict": "unknown", "prompt_status": "missing"},
        )

    contract = (
        f"\n\n{CODER_HANDOFF_FORMAT}"
        f"\n\nCRITICAL FORMATTING INSTRUCTIONS:\n"
        f"1. You MUST include a binding verification line:\n"
        f"   head_sha: {expected_sha}\n\n"
        f"2. You MUST conclude your review with:\n"
        f"   verdict: <pass|fail>\n"
    )
    sha_directive = (
        f"\n\n<!-- RUNNER BINDING REQUIREMENT (non-negotiable) -->\n"
        f"expected_head_sha: {expected_sha}\n\n"
        f"CRITICAL: Your response MUST include the following line verbatim:\n"
        f"head_sha: {expected_sha}\n"
    )
    prompt = rendered + contract + sha_directive
    timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "1200"), 1200)

    # Reviewer backend routing + agy→claude infra fallback live in
    # _execute_gate (shared with _slash_gate / _run_universal_prompt_gate).
    backend, gate_meta = _handlers_shim._resolve_gate_backend(node, ctx)
    result = _handlers_shim._execute_gate(
        prompt, expected_sha, timeout, ctx, name, backend,
        gate_strict=_gate_strict_flag(node),
    )
    if gate_meta:
        for k, v in gate_meta.items():
            result.metadata.setdefault(k, v)
    return result


def _gate_es(node: "Node", ctx: "Context") -> "Result":
    if _node_prompt_ref(node):
        return _run_custom_prompt_gate(node, ctx, "gate_es")
    local_es = ctx.workdir / ".claude" / "commands" / "es.md"
    if local_es.exists():
        return _slash_gate("es")(node, ctx)
    return _run_universal_prompt_gate(UNIVERSAL_EVIDENCE_REVIEW_PROMPT, "gate_es", node, ctx)


def _gate_er(node: "Node", ctx: "Context") -> "Result":
    if _node_prompt_ref(node):
        return _run_custom_prompt_gate(node, ctx, "gate_er")
    local_er = ctx.workdir / ".claude" / "commands" / "er.md"
    local_cmd = ctx.workdir / ".claude" / "commands" / "evidence_review.md"
    if local_er.exists():
        return _slash_gate("er")(node, ctx)
    if local_cmd.exists():
        return _slash_gate("evidence_review")(node, ctx)
    return _run_universal_prompt_gate(UNIVERSAL_EVIDENCE_REVIEW_PROMPT, "gate_er", node, ctx)


def _gate_code_standards(node: "Node", ctx: "Context") -> "Result":
    if _node_prompt_ref(node):
        return _run_custom_prompt_gate(node, ctx, "gate_code_standards")
    local_cmd = ctx.workdir / ".claude" / "commands" / "code-standards.md"
    if local_cmd.exists():
        return _slash_gate("code-standards")(node, ctx)
    return _run_universal_prompt_gate(UNIVERSAL_CODE_STANDARDS_PROMPT, "gate_code_standards", node, ctx)


UNIVERSAL_DEAD_CODE_PROMPT = """\
You are performing an automated Dead Code & Cleanliness Review.
You have full read-write tool access to the current workspace. Proactively use your tools to inspect, compile, check references, and verify the workspace.

You MUST audit the implementation against the following core principles:

1. DEAD CODE DETECTOR:
   - Identify any commented-out code blocks, legacy comments that no longer apply, or debug prints.
   - Look for any unused functions, classes, methods, or imports introduced or left behind in the modified files.
   - Look for any unused variables or constants.

2. CLEANUP INTEGRITY:
   - Ensure old/legacy/dead code has been completely deleted rather than commented out or left dangling.
   - Verify that all newly introduced primitives are actually used.

Provide a detailed review report listing:
- A brief summary of scope.
- Audit results for Dead Code and Cleanup Integrity.
- A bulleted list of any blockers and required fixes.

CRITICAL FORMATTING INSTRUCTIONS:
1. You MUST include a binding verification line:
   head_sha: {expected_sha}

2. You MUST conclude your review with:
   verdict: <pass|fail>
"""


def _gate_dead_code(node: "Node", ctx: "Context") -> "Result":
    if _node_prompt_ref(node):
        return _run_custom_prompt_gate(node, ctx, "gate_dead_code")
    local_cmd = ctx.workdir / ".claude" / "commands" / "dead-code.md"
    if local_cmd.exists():
        return _slash_gate("dead-code")(node, ctx)
    return _run_universal_prompt_gate(UNIVERSAL_DEAD_CODE_PROMPT, "gate_dead_code", node, ctx)


UNIVERSAL_SKEPTIC_PROMPT = """\
You are performing an automated Skeptic Review with INVERTED INCENTIVE.
Your job is NOT to confirm the implementation works. Your job is to find what would FAIL under adversarial scrutiny.

Context: the implementer has reported the change as complete. The runner claims the tests pass.
You are the last line of defense before merge. Assume the implementer is wrong, lazy, or
mistaken until you have direct evidence to the contrary. Probe the claim; do not endorse it.

You have full read-write tool access to the current workspace. Proactively use your tools to
inspect, build, run tests, and actively verify the code under hostile-reading assumptions.

You MUST audit the implementation against the following dimensions — every dimension is a
chance to find a hidden failure, not a checkbox to tick off:

1. EVIDENCE GAPS:
   - What claims are asserted in the diff or commit message that lack a runnable artifact
     (test, log, screenshot, transcript, checksum)?
   - Are the cited test paths, fixture names, or PR numbers actually present in the repo?
   - Does any "verified by X" claim reference a tool, file, or branch that does not exist?

2. HIDDEN ASSUMPTIONS:
   - What does the diff assume is true about the runtime environment, callers, or data
     shape that may not hold in production?
   - Does the change depend on a contract that was never written down (e.g. "the caller
     always passes X")?
   - Does the implementation silently rely on a default value that is not in the spec?

3. RACE CONDITIONS & TIMING:
   - Are there shared mutable state, ordering dependencies, or TOCTOU windows that the
     diff introduces or fails to handle?
   - Does the change touch subprocess / file / network calls whose return values are
     checked but whose side effects are not?

4. EDGE CASES & FAILURE MODES:
   - What happens on empty input, malformed input, oversized input, unicode edge cases,
     permission denied, missing files, partial writes, or network failure?
   - Does the diff add error handling that swallows the original error message or replaces
     it with a generic one?
   - Are there uncaught exceptions in any new code path?

5. CONTRACT DRIFT:
   - Does the implementation quietly change a public signature, return type, error code,
     or default behavior compared to the existing code or the spec it claims to fulfill?
   - Does the diff remove, rename, or repurpose a function/symbol that other files in the
     repo reference (grep before accepting)?
"""


def _gate_skeptic(node: "Node", ctx: "Context") -> "Result":
    # 1. Handle echo/mock_llm backend
    if ctx.backend in ("echo", "mock_llm"):
        hint = ctx.state.get(f"{node.name}.outcome", "success")
        echo_metadata = {"slash_command": "skeptic", "verdict": "echo:" + hint}
        if hint == "inconclusive":
            # Mirror the real handler's issue #827 Defect 1 contract so
            # echo-backend graph tests can exercise gates.dot's
            # `no_verdict`-keyed routing without a real reviewer call.
            echo_metadata["no_verdict"] = "true"
        return Result(
            outcome=hint,
            output=f"echo gate skeptic: pre-seeded {hint}",
            metadata=echo_metadata,
        )

    # 2. Get expected SHA
    expected_sha = _handlers_shim._worktree_head_sha(ctx.workdir)
    if expected_sha is None:
        return Result(
            outcome="error",
            output=f"gate skeptic cannot resolve HEAD SHA for {ctx.workdir}",
            metadata={"slash_command": "skeptic", "verdict": "unknown", "head_sha_status": "missing"},
        )

    # 3. Load rules using RuleLoader
    import os
    import re
    import subprocess
    from pathlib import Path
    
    df_home = Path(os.environ.get("DARK_FACTORY_HOME", "/home/jleechan/projects/dark-factory"))
    global_dir = df_home / "config" / "skeptic"
    local_dir = ctx.workdir / ".claude" / "commands" / "skeptic"
    
    from runner.rule_loader import RuleLoader
    from runner.scm_provider import LocalGitScm
    from runner.dispatcher import VerifierDispatcher
    from runner.consensus import ConsensusAggregator
    from runner.skeptic_gate_cli import get_implementation_identity
    from runner.skeptic_gate import extract_implementation_identity_from_commit
    
    loader = RuleLoader(global_dir, local_dir)
    rules = loader.load_rules()

    if not rules:
        from runner.rule_loader import Rule
        rules = [
            Rule(
                rule_id="universal-skeptic",
                name="Universal Skeptic Reviewer",
                target_globs=["*"],
                model_tier="premium",
                description="Universal skeptic checks",
                prompt=UNIVERSAL_SKEPTIC_PROMPT
            )
        ]
    
    # 4. Resolve PR number
    pr_num = 0
    if ctx.git_ctx and ctx.git_ctx.branch_slug.startswith("pr-"):
        try:
            pr_num = int(ctx.git_ctx.branch_slug.split("-")[1])
        except Exception:
            pass
    if not pr_num:
        pr_env = os.environ.get("GITHUB_PR_NUMBER") or os.environ.get("PR_NUMBER")
        if pr_env:
            try:
                pr_num = int(pr_env)
            except Exception:
                pass
    if not pr_num:
        for k in ("pr_number", "pr"):
            val = ctx.state.get(k)
            if val is not None:
                try:
                    pr_num = int(val)
                    if pr_num:
                        break
                except Exception:
                    pass
    if not pr_num:
        try:
            gh_proc = subprocess.run(
                ["gh", "pr", "view", "--json", "number", "-q", ".number"],
                cwd=str(ctx.workdir),
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
            if gh_proc.returncode == 0 and gh_proc.stdout.strip().isdigit():
                pr_num = int(gh_proc.stdout.strip())
        except Exception:
            pass

    # 5. Resolve repo
    repo = os.environ.get("GITHUB_REPOSITORY")
    if not repo:
        try:
            url_proc = subprocess.run(
                ["git", "config", "--get", "remote.origin.url"],
                cwd=str(ctx.workdir),
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
            if url_proc.returncode == 0 and url_proc.stdout.strip():
                url = url_proc.stdout.strip()
                m = re.search(r"github\.com[:/]([^/]+/[^/.]+?)(?:\.git)?$", url)
                if m:
                    repo = m.group(1)
        except Exception:
            pass
    if not repo:
        repo = "jleechanorg/dark-factory"

    # 6. Resolve diff and changed files using ScmProvider
    scm = LocalGitScm(ctx.workdir)
    target = f"PR:{pr_num}" if pr_num else "HEAD"
    
    try:
        diff = scm.get_diff(target)
        changed_files = scm.get_changed_files(target)
    except Exception as exc:
        return Result(
            outcome="error",
            output=f"gate skeptic failed to resolve SCM context: {exc}",
            metadata={"slash_command": "skeptic", "verdict": "unknown", "scm_status": "failed"},
        )

    # 7. Determine implementation identity
    implementation_identity = "unknown"
    if pr_num:
        try:
            implementation_identity = get_implementation_identity(repo, pr_num, expected_sha)
        except Exception:
            implementation_identity = "unknown"

    if not implementation_identity or implementation_identity == "unknown":
        try:
            subj_proc = subprocess.run(
                ["git", "log", "-1", "--format=%s", expected_sha],
                cwd=str(ctx.workdir),
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
            if subj_proc.returncode == 0 and subj_proc.stdout.strip():
                implementation_identity = extract_implementation_identity_from_commit(subj_proc.stdout.strip())
        except Exception:
            pass

    # Dispatch rules
    dispatcher = VerifierDispatcher()
    results = dispatcher.dispatch(
        rules, changed_files, diff, repo, pr_num, expected_sha, "unknown", implementation_identity
    )
    
    # Aggregate consensus
    aggregator = ConsensusAggregator()
    verdict, body = aggregator.compile_report(
        results, repo, pr_num, expected_sha, expected_sha, implementation_identity
    )
    
    # Issue #827 Defect 1: a `None` verdict means no reviewer produced a
    # parseable structured verdict (ConsensusAggregator.aggregate's
    # `has_invalid` branch) — a distinct event from a reviewer having
    # actually rejected the diff (verdict == "FAIL"). Conflating the two
    # as `outcome="failure"` routed an absent verdict to the `fix` node,
    # which has no real finding to act on. `outcome="inconclusive"` is
    # already a first-class engine outcome token (see runner/_classify.py
    # and runner/handler_verdict.py's `_VERDICT_NORMALIZE`) — reuse it here
    # instead of inventing a new one.
    #
    # `runner.engine_run._run_single_node` normalizes every Result's
    # `.outcome` through `_classify_outcome` before the graph ever sees it
    # for edge routing (`_classify_outcome("inconclusive") == "failure"`,
    # same bucket as a real rejection), so a `.dot` edge condition on the
    # literal `outcome` value cannot distinguish the two. `metadata` is
    # never touched by that normalization, so `no_verdict="true"` is the
    # signal `pipelines/factory/gates.dot` actually routes on.
    if verdict == "PASS":
        outcome = "success"
    elif verdict is None:
        outcome = "inconclusive"
    else:
        outcome = "failure"
    metadata = {"slash_command": "skeptic", "verdict": verdict}
    if verdict is None:
        metadata["no_verdict"] = "true"
    return Result(
        outcome=outcome,
        output=body,
        metadata=metadata,
    )
