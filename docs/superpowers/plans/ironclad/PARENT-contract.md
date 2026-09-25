# dark-factory-c4zhq — first READY parent contract

target_repo: jleechanorg/dark-factory
Environment: jeff-ubuntu; existing c4zhq; factory assigns branch/worktree. This is a saved plan, not execution authority. Artifact root: /home/jleechan/roadmap/af-first-ready-20260913.
Goal: fix permanent branch-registration park telemetry and independently prove actual READY with eight gates and the unchanged 745 protected identities. Parent owns coordination, 0 code lines; TEST owns tests, IMPL owns tick.rs. Dependency: verified P0 admission.
Reuse: read artifact-root beads.json for child IDs, execution-reference.md for literal commands, P0-contract.md, TEST-contract.md, IMPL-contract.md, EVIDENCE-contract.md and c4-exact-edits.md for full instructions. Read overall-goal.md for the overall four criteria. Missing files mean STOP. Existing i92jy owns account repair; btlc0 owns harness repair.
Steps: 1. Independently verify P0; a blocker report is not admission. 2. After execution is requested, run INTAKE from execution-reference.md on Linux; it implements the verified existing-bead exact-store show→label→readback for c4zhq only. 3. Its worker executes TEST-contract.md; a fresh reviewer reproduces committed expected RED and closes TEST. 4. The same worker executes IMPL-contract.md, commits GREEN and runs PUBLISH. The operator repeats MONITOR, with progress updates between its 45-second intervals, while the existing daemon performs the draft-PR/eight-gate loop. 5. A different-family reviewer executes EVIDENCE-contract.md. Close parent only after the children and overall criteria pass together.
RED: TEST reproduces C4_EXPECT_DURABLE_PARK_TELEMETRY at test_sha; compiler failure is not RED.
GREEN: P0, C4-GREEN, OWNERSHIP, E2E and HOLDS from execution-reference.md all pass with actual independent raw-source reproduction.
Do not: do not label children factory; instead sequence them as administrative subtasks in this parent worker. Do not make parent depend on TEST/IMPL; instead link TEST→IMPL→EVIDENCE only among children. Do not assume the worker supports this: P0 must verify contract readability, independent RED review and continuation within prompt/account limits; otherwise stop admission. Do not hand-code, merge, undraft, change AO/wrappers, recover holds or replace services; instead report the failed prerequisite to its owner.
Prior-failure warning: permanent parks were mislabeled transient; a live Go service and environment variable presence did not establish actual routing or account isolation.
Stop when: unexpected command output, one failed identical retry, missing file, instruction conflict, unscoped bootstrap or eight-hour execution deadline. Report failure; parent continues scoped diagnosis. Default verdict FAIL.
Report: run PARENT-REPORT from execution-reference.md. Only the independent reviewer, after every child and overall criterion passes, runs CLOSE-PARENT. Do not create reviewer receipts yourself; P0 and EVIDENCE assign that judgment to their named independent reviewer. A worker status message cannot close the parent.

| Criterion | Check command | External anchor | Independent verifier |
|---|---|---|---|
| Scoped Go execution | P0 from execution-reference.md | Re-executed launch boundary and real child identity | Different-family factory reviewer; default FAIL |
| Correct real outcome | C4-GREEN and E2E from execution-reference.md | Committed tests and live AO/GitHub/overlay/gates | Fresh reviewer reruns; default FAIL |
| Protected state | HOLDS and OWNERSHIP from execution-reference.md | Identity equality and committed diff/ancestry | Fresh reviewer reruns; default FAIL |
