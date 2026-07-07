# Success Criteria — /af E2E proof

Strict mode: all criteria require concrete, independently-verifiable evidence.

## C1 — PR #190 merged fully green (jleechan-sniw.1)
- [ ] Merge/close state checked FIRST (`gh api pulls/190 --jq '{state,merged}'`)
- [ ] All checks SUCCESS at final head SHA (test, daemon-tests, Evidence Gate,
      skeptic, notify, CodeRabbit); Bugbot skip documented
- [ ] No unresolved inline review threads
- [ ] Squashed to single commit before merge
- [ ] PR #190 shows `merged: true`

## C2 — Live /af labeled-PR E2E proven (jleechan-sniw.2)
- [ ] Daemon running as durable service with live `systemctl --user status`
      (active/running) evidence
- [ ] ≥2 watchdog-fed tick intervals visible in `journalctl --user` output
- [ ] A real factory-labeled PR (or canary bead) is adopted by daemon intake
      (telemetry/CXDB record of intake event)
- [ ] AO worker session dispatched by the daemon (not by operator command)
- [ ] Gates run and recorded (evidence in telemetry log / GitHub checks)
- [ ] PR reaches READY/merge state without operator coding intervention
      (operator label-apply and merge-approve allowed per zero-touch defn)
- [ ] Evidence bundle written under /tmp or scratchpad + referenced in bead
      close notes; independent skeptic/evidence-review pass on the bundle

## C3 — Sessions::attach remediation (jleechan-tfs1, P1)
- [ ] Adopted PRs with an existing branch get a real Sessions::attach
      remediation path (code + test) OR bead re-scoped with explicit reason

## C4 — Remaining open PR reconciliation
- [ ] Each of #163, #164, #165, #172, #173, #174, #179 is either merged,
      closed-with-reason, or has an updated bead with concrete next action
- [ ] No open PR left in CONFLICTING state without a rebase bead

## C5 — Method fidelity + tracking
- [ ] /sidekick orchestrator spawned and owns the mission durably
- [ ] /swarm used for adversarial verification fan-out on the E2E claim
- [ ] Beads updated at each transition (in_progress → closed with evidence)
- [ ] Roadmap activity + nextsteps updated at end
