Create `attractor_spec.md` for the requested feature. The attractor spec
describes the **convergence target** — the stable, observable end state
the system must reach for this work to be considered done.

In the StrongDM Attractor pattern, the "attractor" is the goal state the
system converges to. For a `/fs` spec, the attractor is the answer to:
"Once this work is merged, what does success look like as a stable,
observable end state that the system can be measured against?"

Goal:
${goal}

Read `.dark-factory/explore-findings.md` from the explore phase. The
attractor spec must align with its authorities map and centralization
proposal. If the explore artifact is missing, stop and report that — do
not invent a design from scratch.

**Hard requirement — pair with main spec:**
You MUST also read `spec.md` (the main spec) if it exists. The attractor
spec is the **goal-state complement** to the main spec:

| | main spec (spec.md) | attractor spec (attractor_spec.md) |
|---|---|---|
| question | "How do we get there?" | "What does done look like?" |
| describes | implementation path | convergence goal |
| artifact | acceptance criteria, test command, lane matrix | convergence target + verification |
| time horizon | during the work | after the work has merged |

The attractor spec MUST be consistent with `spec.md`. If the main spec
proposes a file-ownership matrix, the attractor spec must reference the
same lanes. If the main spec defines a deterministic test command, the
attractor spec must use the same command as the attractor verification
command. Mismatch is a blocking failure at the attractor review.

Include:

- **Convergence target**: the stable end state the system must reach.
  One sentence, single concrete noun. Example: "A level-up session can
  be applied atomically with all four reducer outputs persisted in one
  transaction, and the apply-level-up signal is owned by the model
  (not synthesized by the server)."

- **Observable convergence criteria**: deterministic checks that prove
  the system has reached the attractor. At least one, preferably a
  small set. Each must be a test command, a metric, a log line, or a
  document shape — something a reviewer can run or observe without
  additional setup.

- **Anti-attractor states**: end states the system MUST NOT converge
  to. At least one. Examples: partial writes, fallback synthesis,
  server-injected choices, dual writers, divergent blobs of the same
  module across lanes, holdout results that pass on the old fallback.

- **Attractor verification command**: a single deterministic command
  that proves the system is at the attractor. If the main spec
  declares a test command, the attractor verification command is the
  same command (consistency). If the main spec is single-lane, the
  attractor verification is single-lane by extension.

- **Distinction from main spec**: a one-paragraph note explaining how
  the attractor spec differs from `spec.md`. Cross-reference specific
  sections of `spec.md` (file paths + line ranges) so a reviewer can
  verify consistency.

- **Evidence expected before merge**: what proof (test output, video,
  evidence bundle path) shows the system has reached the attractor. If
  the main spec declares evidence, the attractor spec must reference
  the same evidence (consistency).

- **Non-attractor states (negative scope)**: a short list of end
  states the system is NOT the attractor. Examples: "the system is
  NOT at the attractor when partial writes exist; the system is NOT
  at the attractor when the server still synthesizes a planning block
  on the prompt-full-sheet path; the system is NOT at the attractor
  when the model never emits a `level_up_signal` field."

Do not write hidden holdout scenarios or evaluator details into the
attractor spec. The runner will execute sealed validation separately.
Do not implement yet.

**Hard requirement — no parallel-lane expansion:**
Do not introduce new parallel lanes as part of the attractor spec. If
the main spec proposes lanes, the attractor spec uses the same lanes.
If the attractor needs additional lane structure, that's a finding
about the main spec — surface it as a comment, do not silently expand.

After writing `attractor_spec.md`, output a one-line confirmation of
its path so the runner can wire it into the next review node.
