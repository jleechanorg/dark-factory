# Retrospective: why a 53-agent adversarial review missed the plan-level flaws — 2026-07-06

## What was missed

The ultracode gap review (`docs/factory-goal-gap-review-2026-07-06.md`) confirmed 42 findings and
produced a ranked blocker list plus a Self-Hosting Ratchet proposal (bead `jleechan-niq`). A
follow-up subagent review (bead `jleechan-ron`, closed) found the ratchet strategically right but
identified flaws none of the 53 agents surfaced:

1. **Ordering hazard:** blocker #1 was "systemd unit with `Restart=always`" while a *confirmed
   finding* said one gh 403 kills the daemon (`main.rs:277 → exit(1)`, no backoff, 10s ticks).
   Auto-restart before per-tick error isolation converts a crash into a **rate-limit-burning crash
   loop**. Isolation/backoff must land first.
2. **Watchdog metric is gameable by a confirmed finding:** the proposed watchdog counted "state
   transitions > 0 per window" — but the review itself documented `recover-held` churn
   (HUMAN_HELD → QUEUED → HUMAN_HELD, unconditional requeue). Raw-transition counting certifies
   that churn as progress. The metric must exclude churn cycles and canary-only movement.
3. **Canary overclaim:** the proposal called a trivial daily canary an "E2E heartbeat." A
   docs-only append exercises intake/dispatch/scheduling but not the code gates or evidence path —
   by our own evidence-standards rules it is a **liveness smoke**, not E2E, until it proves a full
   PR lifecycle with the required evidence class.
4. **No promotion criteria:** "hand blockers 4+ to the factory" had no gate. Refined: 3 consecutive
   canary successes + at least one non-canary bead autonomously escaping HUMAN_HELD before any
   handoff, and only low-risk blockers first.
5. **No self-modification guardrail:** nothing forbade the factory from mutating its own
   watchdog/canary/supervisor/evidence rules while working its own blocker beads. Oversight
   components must be write-locked to the factory until the ratchet has history.
6. **Missing instrumentation:** a zero-touch ledger with distinct buckets (pure zero-touch /
   watchdog-assisted / human-assisted / failed-stalled) and daemon↔runner correlation (bead id,
   branch, PR, runner run id, head SHA, evidence bundle hash) — without which the cutover metric
   is unmeasurable and gate verdicts stay ungrounded.

## Why the process missed it (5 Whys, technical + path)

**Why #1 — the adversarial machinery pointed backward, not forward.** Every refutation agent
attacked *findings about the current state* ("is this gap real?"). The blocker ranking and the
ratchet proposal — the artifacts that actually direct future work — received **zero adversarial
passes**. The process spent ~4M tokens verifying the diagnosis and none verifying the prescription.

**Why #2 — synthesis was a single point of failure.** One agent turned 42 findings into a ranked
plan. Ranking requires *cross-finding interaction analysis* (finding "daemon dies on 403" ×
blocker "Restart=always" ⇒ crash loop), which is a different cognitive task from confirming
findings one at a time. No agent owned interactions; the per-finding refuters structurally
couldn't see them.

**Why #3 — /innov ran solo, inline, immediately after a "42/46 survived" result.** The innovation
was authored in one pass with no refutation lens, in a context primed by survivorship ("our
findings held up"). The clearest evidence that this was a process failure rather than a knowledge
failure: **both missed items were already in context.** The crash-loop prerequisite appeared in the
same message's brainstorm section ("Restart=always without it is a crash loop") but was never
reconciled with the ranked list; the churn finding that games the watchdog metric was confirmed
finding material. Information present ≠ information applied — only an agent explicitly tasked
"attack this plan using the confirmed findings as ammunition" reliably closes that gap.

**Why #4 — no self-reference threat model.** All six review dimensions were present-state audits.
The self-modification guardrail requires modeling the *future* system in which the factory's work
items include the factory's own oversight — a question the dimension prompts never posed. The
repo's isolation discipline (implementing agent must not read holdouts) is exactly this class of
rule, but nobody generalized it to watchdog/canary/supervisor artifacts.

**Why #5 — our own artifacts were exempt from our own gates.** The repo enforces evidence-class
honesty for tests ("if a claim is weaker than the name suggests, rename it") and the session had
just documented the gate-self-certification anti-pattern — yet the "canary = E2E" label and the
gameable watchdog metric shipped unreviewed. The review process had no step that turns the
standards we apply to the factory onto the review's own outputs.

## Harness fixes (so this class of miss can't recur)

1. **Plan-refutation phase in review workflows:** after synthesis, spawn adversarial agents whose
   prompt is "attack the remediation *ordering and proposals* using the confirmed findings as
   ammunition; find any blocker whose fix is hazardous before another lands, and any proposed
   metric/gate a confirmed finding can game." (Applied: this is now the template for future
   ultracode reviews in this repo.)
2. **/innov output is never final:** any innovation that files a bead gets one adversarial
   subagent pass (or /advice) before the bead is created — same rule the factory applies to code
   via reviewer nodes, applied to plans.
3. **Metric-gaming check:** every proposed metric/watchdog/gate must answer "which known behavior
   satisfies this metric while doing no useful work?" before adoption (churn, canary-only motion,
   sentinel-substring matches are the standing examples).
4. **Self-reference rule for autonomy plans:** enumerate oversight components (watchdog, canary
   definitions, supervisor units, evidence rules, verifier prompts) and write-lock them against
   the autonomous system until it has a promotion history.
5. **Reconciliation step:** when a session produces both a ranked plan and a brainstorm/appendix,
   diff them — any prerequisite named in the appendix but absent from the ranking is a defect, not
   a footnote.

## Refined ratchet sequence (supersedes the ordering inside bead jleechan-niq)

1. Per-tick error isolation + backoff (`jleechan-qdw`, promoted ahead of scheduling) → then
   systemd `--user` unit with `Restart=always` (`jleechan-1m4`).
2. Router fallback fix (`jleechan-g1k`) and HUMAN_HELD recovery (`jleechan-gib`).
3. **Read-only** watchdog first — out-of-band alerting, deduped incidents, meaningful-progress
   metric (churn and canary-only movement excluded).
4. Daily canary, classified as **liveness smoke**; upgrade its label only when it proves the full
   PR lifecycle with the required evidence class.
5. Promotion gate: 3 consecutive canary successes + one non-canary bead escaping HUMAN_HELD
   autonomously.
6. Only then hand **low-risk** blockers to the factory; oversight components stay write-locked.
7. Zero-touch ledger (four buckets) + daemon↔runner correlation IDs from day one.

## Pointers

- Gap review: `docs/factory-goal-gap-review-2026-07-06.md`
- Reconciled roadmap: `roadmap/nextsteps-2026-07-06-gap-review.md`
- Ratchet bead: `jleechan-niq` (sequence above supersedes its embedded ordering)
- Review bead: `jleechan-ron` (closed — subagent review, no files edited)
- Related memory: gate-self-certification anti-pattern; roadmap overclaim pattern
