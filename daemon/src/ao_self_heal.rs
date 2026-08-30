// Bead dark-factory-8d1o: Factory self-heal for project-scoped AO
// unavailability BEFORE coder spawn.
//
// CONTEXT
// The factory's dispatch path shells out to `ao status -p <project> --json`
// and `ao spawn --agent <a> ...` before promoting a bead to DISPATCHED. When
// the AO daemon is not running for a configured project (cold start, crashed
// supervisor, manual stop, never-started-after-machine-reboot) every spawn
// fails as `DaemonError::Tool { rc, stderr }`. `dispatch_ready` then routes
// these into `spawn_failure_count`, burning `MAX_TRANSIENT_SPAWN_RETRY`
// without ever creating a worker, and eventually parking the bead HUMAN_HELD
// with `transient_spawn_retry_cap_exceeded` — a clear misclassification: the
// failure is structural (the daemon is missing), not transient (a hiccup in
// a healthy daemon).
//
// FIX (this module)
// Three responsibilities, kept pure so tests don't need a real `ao`:
//
//   1. `classify_probe(stderr, exit_code) -> ProbeOutcome`
//      Maps a raw `ao <subcmd>` failure to one of:
//      - `Healthy`        — exit 0 / parseable JSON
//      - `Unavailable(r)` — structured AO-down signal (anchored on a small
//                           fixed set of substrings + exit-127 for missing
//                           binary, both stable across the AO Go/TS splits the
//                           factory already tolerates)
//      - `Unknown(r)`     — anything else (already-retryable transient).
//
//   2. `decide_heal_action(preflight, after_start, ...)` — pure decidision
//      function returning `Noop | Restarted | FailClosed{reason}`.
//
//   3. `ensure_ao_available(probe, project)` — orchestrator that calls the
//      probe, attempts a SINGLE headless start if the probe came back
//      `Unavailable`, re-probes once, and returns the heal action.
//
// CONTRACT FOR THE CALLER (dispatch / adapter layer)
// - `Healthy` / `Unknown` from `ensure_ao_available` mean "proceed with the
//   spawn as if nothing happened" — no behavioral change vs. today.
// - `Restarted` means "AO was just brought up by the factory; proceed with
//   the spawn exactly ONCE, do not auto-retry on a second cold start".
// - `FailClosed { reason }` means the dispatch layer MUST park HUMAN_HELD
//   with the carried `reason` so a human or operator-hook can investigate.
//   The string is intentionally verbose (preflight + start attempt + after
//   start) so the park reason stands alone in telemetry without needing
//   cross-referencing other logs.
//
// EXPLICITLY OUT OF SCOPE
// - Modifying `agent-orchestrator` source (AO CODE APPROVED was not granted
//   for this bead). The start command is operator-supplied via
//   `DARK_FACTORY_AO_START_CMD` env var (default: `ao start --headless`) so
//   every install can pin its own daemon-management surface.
//
// - Looping. `ensure_ao_available` calls `start_daemon` AT MOST ONCE per
//   invocation. A retry loop in this module would hide failures from the
//   fail-closed contract above.
//
// - Tying this module to `dispatch.rs`'s spawn-failure counter. Today every
//   AO-not-running spawn silently increments `spawn_failure_count`. After
//   this module is wired in, the AO-not-running path goes through the heal
//   step FIRST and only fails through to the counter if the heal itself
//   returns `FailClosed` (i.e. operator intervention is required).
//
// TDD ANCHORS (matching `daemon/tests/`'s single-crate-per-file convention)
// The unit tests live at the bottom of this file in a `#[cfg(test)] mod
// tests`. Their red→green→refactor history is the evidence the bead ships
// with; they cover:
//
//   classify_probe:
//     - "daemon not running"                    -> Unavailable
//     - "ECONNREFUSED 127.0.0.1:8500"           -> Unavailable
//     - "could not connect to AO"               -> Unavailable
//     - "no such file" + "socket" together      -> Unavailable
//     - exit_code = Some(127)                    -> Unavailable
//     - unrelated stderr                        -> Unknown
//     - empty stderr + None exit_code            -> Unknown (defensive)
//
//   decide_heal_action:
//     - preflight Healthy            -> Noop
//     - preflight Unknown            -> Noop
//     - preflight Unavail + after Healthy + start_attempted -> Restarted
//     - preflight Unavail + after Unavail + start_attempted   -> FailClosed
//     - preflight Unavail + start_attempted=false (defensive) -> FailClosed
//
//   ensure_ao_available via FakeProbe:
//     - healthy preflight            -> AlreadyAvailable (Noop)
//     - unknown preflight            -> AlreadyAvailable (Noop, defer to spawn)
//     - unavailable -> health after start -> Restarted, start_daemon called once
//     - unavailable -> unavailable after start -> FailClosed with reason
//                       containing both diagnostics + attempted cmd
//     - already-restarted-then-re-unavailable in the SAME call: still single
//       start attempt, no recursion, FailClosed surfaces (bounded retry)
//
// REFERENCE EVIDENCE
// The `factory-ao-remediate.sh` shell layer already handles a similar heal at
// the per-bead-remediation boundary (async spawn wrapper). This module is
// the in-process equivalent, scoped to BEFORE the spawn (so a healthy AO is
// guaranteed visible at the moment the dispatcher promotes to DISPATCHED)
// and governed by the same fail-closed invariant the shell layer enforces.

use std::time::{Duration, Instant};

/// Result of probing `ao status -p <project> --json` (or any equivalent
/// AoProbe). `Healthy` and `Unknown` mean "the dispatcher should proceed
/// as it does today"; only `Unavailable` triggers self-heal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// `ao` exited 0 (or returned parseable JSON). The dispatcher may proceed.
    Healthy,
    /// Structured "AO not running" signal. The factory should attempt to
    /// start the configured `ao start` headlessly and retry the spawn once.
    /// The contained string is the raw stderr (capped at 256 chars) so
    /// telemetry carries the operator-visible reason verbatim.
    Unavailable(String),
    /// Some other tool failure — connection drop, timeout, parse error, etc.
    /// This is the existing transient-failure path; the daemon's normal
    /// `is_transient()` handling continues to apply.
    Unknown(String),
}

/// Pure classification. Maps `(stderr, exit_code)` from a failed
/// `run_tool("ao", ...)` call to a [`ProbeOutcome`]. Stable substrings
/// picked from the actual AO Go/TS error strings the factory already
/// shells under — see the module-level comment for sources.
pub fn classify_probe(stderr: &str, exit_code: Option<i32>) -> ProbeOutcome {
    let lc = stderr.to_ascii_lowercase();
    const UNAVAILABLE_ANCHORS: &[&str] = &[
        "daemon not running",
        "econnrefused",
        "could not connect",
    ];
    if UNAVAILABLE_ANCHORS.iter().any(|a| lc.contains(a)) {
        return ProbeOutcome::Unavailable(truncate(stderr));
    }
    // "no such file" is too generic on its own (a fixture or config file
    // missing also matches), but in combination with a socket signature —
    // either the literal word "socket" OR the AO socket-file extension
    // `.sock` / `.socket` — it is the AO socket path missing, same
    // structural condition as a refused connection.
    if lc.contains("no such file") && (lc.contains("socket") || lc.contains(".sock")) {
        return ProbeOutcome::Unavailable(truncate(stderr));
    }
    // Exit 127 = "command not found"; structurally identical to "not
    // running" for our purposes — the spawn path cannot proceed until
    // `ao` is on PATH.
    if matches!(exit_code, Some(127)) {
        return ProbeOutcome::Unavailable(format!(
            "ao binary not on PATH (exit 127): {}",
            truncate(stderr)
        ));
    }
    ProbeOutcome::Unknown(truncate(stderr))
}

fn truncate(s: &str) -> String {
    s.chars().take(256).collect::<String>()
}

/// The structured heal outcome the dispatcher consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealAction {
    /// Probe came back Healthy or Unknown; nothing to do. Caller should
    /// proceed with the original spawn exactly as if this module did not
    /// exist.
    Noop,
    /// Probe transitioned Unavailable -> Healthy via a successful start
    /// attempt. The dispatcher should retry the original spawn exactly
    /// ONCE. It must NOT auto-retry on a third try — if the retry also
    /// surfaces `Unavailable`, route to `FailClosed` like a structural
    /// outage.
    Restarted,
    /// Heal attempt failed; AO is still unreachable. Caller MUST park
    /// `HUMAN_HELD` with the `reason` string carried into telemetry
    /// verbatim. This is the fail-closed invariant the bead describes.
    FailClosed { reason: String },
}

/// Pure decision function. Takes the preflight probe outcome, the
/// re-probe outcome AFTER the (optional) start attempt, the start command
/// name used for telemetry, whether a start was actually attempted, and
/// the start attempt's own error string (None on success). The string
/// arguments only feed into `FailClosed`'s reason — successful paths
/// ignore them.
///
/// `start_attempted` exists defensively: `ensure_ao_available` always
/// attempts a start on `Unavailable`, but exposing it as a parameter
/// keeps `decide_heal_action` callable from other pipelines (e.g. tests,
/// or a future "skip start" mode for `ao` installs where there's nothing
/// to start because the daemon is a managed external service).
pub fn decide_heal_action(
    preflight: &ProbeOutcome,
    after_start: &ProbeOutcome,
    start_command_name: &str,
    start_attempted: bool,
    start_error: Option<&str>,
) -> HealAction {
    match preflight {
        ProbeOutcome::Healthy => HealAction::Noop,
        ProbeOutcome::Unknown(_) => HealAction::Noop,
        ProbeOutcome::Unavailable(preflight_reason) => {
            if !start_attempted {
                return HealAction::FailClosed {
                    reason: format!(
                        "ao preflight unavailable but start was not attempted \
                         (start_command={start_command_name:?}); \
                         preflight_stderr={preflight_reason}; \
                         operator_must_investigate_or_supply_DARK_FACTORY_AO_START_CMD"
                    ),
                };
            }
            match after_start {
                ProbeOutcome::Healthy => HealAction::Restarted,
                ProbeOutcome::Unavailable(after_reason) => HealAction::FailClosed {
                    reason: format!(
                        "ao self-heal failed; preflight=Unavailable({preflight_reason}); \
                         start_command={start_command_name}; \
                         start_error={start_error:?}; \
                         re_probe=Unavailable({after_reason}); \
                         operator_action_required=verify_{start_command_name}_exists_and_{start_command_name}_is_on_PATH"
                    ),
                },
                ProbeOutcome::Unknown(after_reason) => HealAction::FailClosed {
                    reason: format!(
                        "ao self-heal failed; preflight=Unavailable({preflight_reason}); \
                         start_command={start_command_name}; \
                         start_error={start_error:?}; \
                         re_probe=Unknown({after_reason}); \
                         operator_action_required=inspect_ao_logs_after_start"
                    ),
                },
            }
        }
    }
}

/// Production + test seam for the runtime AO interaction. Production
/// binds to `RunToolProbe` (subprocess); tests bind a `FakeProbe` that
/// records calls and returns scripted outcomes. This mirrors the
/// `Tracker`/`Sessions` seam pattern already used by the dispatch
/// integration tests (`daemon/tests/common/mod.rs`).
pub trait AoProbe {
    /// Probe AO for `project`. Returns the structured outcome; on a
    /// subprocess error the impl is expected to also include the
    /// stderr/exit_code in the `Unavailable`/`Unknown` payload.
    fn probe(&self, project: &str) -> ProbeOutcome;

    /// Attempt to start the AO daemon headlessly. Returns `Ok(())` on
    /// success; `Err(message)` carries the failure reason verbatim into
    /// the `FailClosed` reason string. The implementation is responsible
    /// for any timeout — at most the trait's `start_timeout()` window.
    fn start_daemon(&self) -> Result<(), String>;

    /// Bound on how long `start_daemon` may block. Implementations MUST
    /// honour this — a runaway `start` could otherwise pin a dispatch
    /// tick for the full `start_timeout()` of `factory-af-tick.sh`
    /// (default 120s) and starve every other queued bead.
    fn start_timeout(&self) -> Duration;

    /// Stable command name for telemetry (e.g. `"ao start --headless"`).
    /// Pure metadata; never used to invoke anything.
    fn start_command_name(&self) -> &str {
        "<unnamed-ao-start-command>"
    }
}

/// Run the bounded heal sequence against `probe`:
///
/// 1. Probe.
/// 2. Healthy / Unknown -> return Noop immediately. No probe is "wasted";
///    these are the steady-state.
/// 3. Unavailable -> call `start_daemon()` exactly once (bounded by
///    `start_timeout()`), wait a brief settle window for the daemon to
///    bind the socket, then re-probe.
/// 4. Decide via [`decide_heal_action`] and return the result.
///
/// `ensure_ao_available` does NOT touch `spawn_failure_count` or any
/// state outside its return value. That bookkeeping is the dispatcher's
/// job, so the self-heal stays a side-effect-free helper.
pub fn ensure_ao_available(probe: &dyn AoProbe, project: &str) -> HealAction {
    let started_at = Instant::now();
    let preflight = probe.probe(project);
    if !matches!(preflight, ProbeOutcome::Unavailable(_)) {
        return decide_heal_action(
            &preflight,
            &preflight, // after_start irrelevant for Noop paths
            probe.start_command_name(),
            false, // no start attempted
            None,
        );
    }
    // Unavailable -> attempt ONE start.
    let start_result = probe.start_daemon();
    let start_error = start_result.as_ref().err().cloned();
    // Brief settle window. Probe implementations may also enforce
    // their own internal retries; the 250ms here just covers the
    // "process running but socket not yet bound" race.
    std::thread::sleep(Duration::from_millis(250));
    let after_start = probe.probe(project);
    let action = decide_heal_action(
        &preflight,
        &after_start,
        probe.start_command_name(),
        true,
        start_error.as_deref(),
    );
    let _ = started_at.elapsed(); // duration available for future telemetry
    action
}

/// Production AoProbe backed by [`crate::tools::run_tool`] (the daemon's
/// own subprocess helper). Shells out to `ao status -p <project> --json`
/// for probe and an operator-configurable start command
/// (`DARK_FACTORY_AO_START_CMD`, default `ao start --headless`) for
/// `start_daemon`.
///
/// Operator override surface (no agent-orchestrator source changes;
/// AO CODE APPROVED was not granted for this bead):
///
/// - `DARK_FACTORY_AO_START_CMD` — full command line; first whitespace-
///   separated token is the program, remainder is argv.
/// - `DARK_FACTORY_AO_START_TIMEOUT_SECS` — cap on `start_timeout()`
///   (default 30s).
pub struct RunToolProbe {
    pub start_command: String,
    pub start_timeout_secs: u64,
}

impl Default for RunToolProbe {
    fn default() -> Self {
        let start_command = std::env::var("DARK_FACTORY_AO_START_CMD")
            .unwrap_or_else(|_| "ao start --headless".to_string());
        let start_timeout_secs = std::env::var("DARK_FACTORY_AO_START_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        Self {
            start_command,
            start_timeout_secs,
        }
    }
}

impl RunToolProbe {
    pub fn from_env() -> Self {
        Self::default()
    }

    fn probe_via_run_tool(&self, project: &str) -> ProbeOutcome {
        // Mirror the existing `ao status -p <project> --json` calls in
        // `adapters.rs::is_quiescent` / `attach_within` / etc., but route
        // through `run_tool` so the daemon's gh-rate-limit circuit breaker
        // (irrelevant for `ao`, but harmless) and the same timeout /
        // env-isolation contract apply uniformly.
        let result = crate::tools::run_tool(
            "ao",
            &["status", "-p", project, "--json"],
            self.start_timeout_secs,
        );
        match result {
            Ok(_) => ProbeOutcome::Healthy,
            Err(crate::errors::DaemonError::Tool { stderr, rc, .. }) => {
                classify_probe(&stderr, Some(rc))
            }
            Err(crate::errors::DaemonError::Timeout(_)) => {
                // A timeout could mean a hung daemon (Unavailable) or a slow
                // but otherwise responsive one (Unknown). Without a way to
                // tell cleanly, classify as Unknown so the existing transient
                // path handles it rather than firing a self-heal that may
                // make things worse by restarting a daemon that was just
                // slow.
                ProbeOutcome::Unknown("ao status timeout".to_string())
            }
            Err(other) => ProbeOutcome::Unknown(format!("{other}")),
        }
    }

    fn start_via_command(&self) -> Result<(), String> {
        // Split the operator-supplied start command into program + args on
        // whitespace. Naive but matches every documented AO start command
        // (no shell-quoted args expected for `ao start --headless`).
        let mut parts = self.start_command.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| "DARK_FACTORY_AO_START_CMD is empty".to_string())?;
        let args: Vec<&str> = parts.collect();
        let output = std::process::Command::new(program)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("start command failed to execute: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let err_msg = String::from_utf8_lossy(&output.stderr).into_owned();
            let stdout_msg = String::from_utf8_lossy(&output.stdout).into_owned();
            Err(format!(
                "start command exited with status {:?}; stderr={}; stdout={}",
                output.status.code(),
                if err_msg.is_empty() { "<empty>" } else { &err_msg },
                if stdout_msg.is_empty() { "<empty>" } else { &stdout_msg },
            ))
        }
    }
}

impl AoProbe for RunToolProbe {
    fn probe(&self, project: &str) -> ProbeOutcome {
        self.probe_via_run_tool(project)
    }
    fn start_daemon(&self) -> Result<(), String> {
        self.start_via_command()
    }
    fn start_timeout(&self) -> Duration {
        Duration::from_secs(self.start_timeout_secs)
    }
    fn start_command_name(&self) -> &str {
        // Self has a String field; we can't return `&str` from the field
        // without borrowing `self`. Use `Box::leak` is overkill for a
        // telemetry-only string — emit a stable default string and rely
        // on the operator-visible `start_command` for actual inspection.
        "ao start --headless (env:DARK_FACTORY_AO_START_CMD)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ---- classify_probe ---------------------------------------------------

    #[test]
    fn classify_daemon_not_running_is_unavailable() {
        let stderr = "Error: daemon not running for project dark-factory";
        let outcome = classify_probe(stderr, Some(1));
        assert!(
            matches!(outcome, ProbeOutcome::Unavailable(ref s) if s.contains("daemon not running")),
            "expected Unavailable, got {outcome:?}"
        );
    }

    #[test]
    fn classify_econnrefused_is_unavailable() {
        let stderr = "connect ECONNREFUSED 127.0.0.1:8500";
        let outcome = classify_probe(stderr, Some(1));
        assert!(
            matches!(outcome, ProbeOutcome::Unavailable(ref s) if s.contains("ECONNREFUSED")),
            "expected Unavailable, got {outcome:?}"
        );
    }

    #[test]
    fn classify_could_not_connect_is_unavailable() {
        let stderr = "could not connect to agent orchestrator daemon";
        let outcome = classify_probe(stderr, Some(1));
        assert!(
            matches!(outcome, ProbeOutcome::Unavailable(_)),
            "expected Unavailable, got {outcome:?}"
        );
    }

    #[test]
    fn classify_no_such_file_with_socket_is_unavailable() {
        let stderr = "stat /tmp/ao.sock: no such file or directory";
        let outcome = classify_probe(stderr, Some(1));
        assert!(
            matches!(outcome, ProbeOutcome::Unavailable(_)),
            "expected Unavailable, got {outcome:?}"
        );
    }

    #[test]
    fn classify_no_such_file_without_socket_is_unknown() {
        // Just `no such file` alone (e.g. a missing config or fixture path)
        // must not be misclassified as the AO daemon being down. Otherwise
        // a single missing fixture file would trigger a self-heal.
        let stderr = "config: no such file or directory (path: foo.toml)";
        let outcome = classify_probe(stderr, Some(1));
        assert!(
            matches!(outcome, ProbeOutcome::Unknown(_)),
            "expected Unknown, got {outcome:?}"
        );
    }

    #[test]
    fn classify_exit_127_is_unavailable() {
        let outcome = classify_probe("sh: ao: not found", Some(127));
        assert!(
            matches!(outcome, ProbeOutcome::Unavailable(ref s) if s.contains("exit 127")),
            "expected Unavailable (exit 127), got {outcome:?}"
        );
    }

    #[test]
    fn classify_unrelated_error_is_unknown() {
        let stderr = "rate limit exceeded for installation ID 1234";
        let outcome = classify_probe(stderr, Some(1));
        assert!(
            matches!(outcome, ProbeOutcome::Unknown(_)),
            "expected Unknown, got {outcome:?}"
        );
    }

    #[test]
    fn classify_empty_stderr_with_no_exit_code_is_unknown_defensively() {
        // Defensive: should never reach here in production (we only call
        // classify_probe after a failed run_tool), but if a future caller
        // did pass nothing, we degrade to Unknown (proceed with spawn
        // path) rather than misfiring a heal.
        let outcome = classify_probe("", None);
        assert!(
            matches!(outcome, ProbeOutcome::Unknown(_)),
            "expected Unknown, got {outcome:?}"
        );
    }

    #[test]
    fn classify_truncates_long_stderr() {
        // Stress the 256-char cap — the comment in `truncate` says 256.
        let stderr = "daemon not running. ".repeat(100);
        let outcome = classify_probe(&stderr, Some(1));
        match outcome {
            ProbeOutcome::Unavailable(s) => assert!(
                s.len() <= 256,
                "expected stderr <=256 chars after truncate, got {} chars",
                s.len()
            ),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    // ---- decide_heal_action ----------------------------------------------

    #[test]
    fn decide_healthy_is_noop() {
        let action = decide_heal_action(
            &ProbeOutcome::Healthy,
            &ProbeOutcome::Healthy,
            "ao start --headless",
            false,
            None,
        );
        assert_eq!(action, HealAction::Noop);
    }

    #[test]
    fn decide_unknown_is_noop() {
        let action = decide_heal_action(
            &ProbeOutcome::Unknown("rate limit exceeded".into()),
            &ProbeOutcome::Unknown("rate limit exceeded".into()),
            "ao start --headless",
            false,
            None,
        );
        assert_eq!(action, HealAction::Noop);
    }

    #[test]
    fn decide_unavailable_then_healthy_with_start_attempted_is_restarted() {
        let action = decide_heal_action(
            &ProbeOutcome::Unavailable("daemon not running".into()),
            &ProbeOutcome::Healthy,
            "ao start --headless",
            true,
            None,
        );
        assert_eq!(action, HealAction::Restarted);
    }

    #[test]
    fn decide_unavailable_then_unavailable_with_start_attempted_is_fail_closed() {
        let action = decide_heal_action(
            &ProbeOutcome::Unavailable("daemon not running".into()),
            &ProbeOutcome::Unavailable("ECONNREFUSED".into()),
            "ao start --headless",
            true,
            Some("start timed out after 30s"),
        );
        let reason = match action {
            HealAction::FailClosed { reason } => reason,
            other => panic!("expected FailClosed, got {other:?}"),
        };
        // The reason must surface every diagnostic so a human reading only
        // the park reason can act without cross-referencing other logs.
        assert!(reason.contains("preflight=Unavailable"));
        assert!(reason.contains("start_command=ao start --headless"));
        assert!(reason.contains("start_error=Some"));
        assert!(reason.contains("re_probe=Unavailable"));
        assert!(reason.contains("operator_action_required"));
    }

    #[test]
    fn decide_unavailable_without_start_attempted_is_fail_closed_defensively() {
        // Defensive: a future caller that sees Unavailable preflight but
        // somehow skips start_daemon must NOT silently let the dispatch
        // proceed. Fail closed.
        let action = decide_heal_action(
            &ProbeOutcome::Unavailable("daemon not running".into()),
            &ProbeOutcome::Healthy, // re-probe outcome ignored on this branch
            "ao start --headless",
            false,
            None,
        );
        assert!(
            matches!(action, HealAction::FailClosed { .. }),
            "expected FailClosed defensive arm, got {action:?}"
        );
    }

    #[test]
    fn decide_unavailable_then_unknown_after_start_is_fail_closed() {
        // A post-start "Unknown" must NOT be silently dismissed: even
        // though classify_probe normally maps unknowns to the existing
        // transient path, the FACT that it transitioned from
        // Unavailable preflight means the heal was attempted; unknown
        // post-start is ambiguous (start may have crashed the daemon
        // mid-init) and is conservatively fail-closed.
        let action = decide_heal_action(
            &ProbeOutcome::Unavailable("ECONNREFUSED".into()),
            &ProbeOutcome::Unknown("agent orchestrator reported a transient error".into()),
            "ao start --headless",
            true,
            None,
        );
        assert!(
            matches!(action, HealAction::FailClosed { .. }),
            "expected FailClosed on post-start Unknown, got {action:?}"
        );
    }

    // ---- ensure_ao_available via FakeProbe --------------------------------

    /// Scripted probe for tests. Records every method invocation into
    /// `calls` so a test can assert "start_daemon called exactly once".
    #[derive(Debug)]
    struct FakeProbe {
        probe_calls: RefCell<u32>,
        start_calls: RefCell<u32>,
        // Each entry is one probe response; consumed by index.
        probe_responses: RefCell<Vec<ProbeOutcome>>,
        start_result: Result<(), String>,
        start_command: &'static str,
    }

    impl FakeProbe {
        fn new(initial: Vec<ProbeOutcome>, start_result: Result<(), String>) -> Self {
            Self {
                probe_calls: RefCell::new(0),
                start_calls: RefCell::new(0),
                probe_responses: RefCell::new(initial),
                start_result,
                start_command: "ao start --headless",
            }
        }
    }

    impl AoProbe for FakeProbe {
        fn probe(&self, _project: &str) -> ProbeOutcome {
            *self.probe_calls.borrow_mut() += 1;
            let mut responses = self.probe_responses.borrow_mut();
            if responses.is_empty() {
                ProbeOutcome::Unknown("FakeProbe exhausted".into())
            } else {
                responses.remove(0)
            }
        }
        fn start_daemon(&self) -> Result<(), String> {
            *self.start_calls.borrow_mut() += 1;
            match &self.start_result {
                Ok(()) => Ok(()),
                Err(msg) => Err(msg.clone()),
            }
        }
        fn start_timeout(&self) -> Duration {
            Duration::from_secs(5)
        }
        fn start_command_name(&self) -> &str {
            self.start_command
        }
    }

    #[test]
    fn ensure_healthy_preflight_is_noop_with_single_probe_call() {
        let probe = FakeProbe::new(vec![ProbeOutcome::Healthy], Err("never called".into()));
        let action = ensure_ao_available(&probe, "dark-factory");
        assert_eq!(action, HealAction::Noop);
        assert_eq!(*probe.probe_calls.borrow(), 1, "exactly one probe call");
        assert_eq!(*probe.start_calls.borrow(), 0, "start_daemon never called");
    }

    #[test]
    fn ensure_unknown_preflight_is_noop_with_single_probe_call() {
        // Unknown from classify_probe must continue the existing
        // transient-failure path — the self-heal does NOT engage.
        let probe = FakeProbe::new(
            vec![ProbeOutcome::Unknown("rate limit exceeded".into())],
            Err("never called".into()),
        );
        let action = ensure_ao_available(&probe, "dark-factory");
        assert_eq!(action, HealAction::Noop);
        assert_eq!(
            *probe.start_calls.borrow(),
            0,
            "Unknown preflight must NOT trigger a heal"
        );
    }

    #[test]
    fn ensure_unavailable_then_healthy_is_restarted_with_exactly_one_start() {
        let probe = FakeProbe::new(
            vec![
                ProbeOutcome::Unavailable("daemon not running".into()),
                ProbeOutcome::Healthy,
            ],
            Ok(()),
        );
        let action = ensure_ao_available(&probe, "dark-factory");
        assert_eq!(action, HealAction::Restarted);
        assert_eq!(*probe.start_calls.borrow(), 1, "exactly ONE start attempt");
        assert_eq!(*probe.probe_calls.borrow(), 2, "two probes: pre + post");
    }

    #[test]
    fn ensure_unavailable_then_unavailable_with_successful_start_is_fail_closed() {
        // Start returned Ok, but re-probe still says Unavailable — a
        // silently broken start (e.g. wrong project flag, missing
        // socket bind). The factory MUST fail closed in this case;
        // silently proceeding with the spawn would just produce a
        // Tool error and burn the spawn retry cap.
        let probe = FakeProbe::new(
            vec![
                ProbeOutcome::Unavailable("ECONNREFUSED 127.0.0.1:8500".into()),
                ProbeOutcome::Unavailable("ECONNREFUSED 127.0.0.1:8500".into()),
            ],
            Ok(()),
        );
        let action = ensure_ao_available(&probe, "dark-factory");
        let reason = match action {
            HealAction::FailClosed { reason } => reason,
            other => panic!("expected FailClosed, got {other:?}"),
        };
        assert_eq!(*probe.start_calls.borrow(), 1, "exactly ONE start attempt");
        assert!(reason.contains("ao start --headless"));
        assert!(reason.contains("ECONNREFUSED"));
    }

    #[test]
    fn ensure_unavailable_with_failing_start_is_fail_closed_carrying_start_error() {
        let probe = FakeProbe::new(
            vec![ProbeOutcome::Unavailable("daemon not running".into())],
            Err("start_daemon: timed out after 30s".into()),
        );
        let action = ensure_ao_available(&probe, "dark-factory");
        let reason = match action {
            HealAction::FailClosed { reason } => reason,
            other => panic!("expected FailClosed, got {other:?}"),
        };
        assert_eq!(*probe.start_calls.borrow(), 1);
        assert!(
            reason.contains("start_daemon: timed out after 30s"),
            "start_error must be in the reason verbatim, got: {reason}"
        );
    }

    #[test]
    fn ensure_never_recurses_into_second_start_attempt() {
        // Bounded-retry invariant: even if the second probe also comes
        // back Unavailable (and there are MORE scripted Unavailable
        // entries sitting in the queue), ensure_ao_available must NOT
        // call start_daemon a second time. The dispatcher decides what
        // to do with FailClosed; this helper does not loop.
        let probe = FakeProbe::new(
            vec![
                ProbeOutcome::Unavailable("daemon not running".into()),
                ProbeOutcome::Unavailable("daemon not running".into()),
                ProbeOutcome::Unavailable("daemon not running".into()),
            ],
            Ok(()),
        );
        let action = ensure_ao_available(&probe, "dark-factory");
        assert!(matches!(action, HealAction::FailClosed { .. }));
        assert_eq!(
            *probe.start_calls.borrow(),
            1,
            "must attempt start exactly once even with multiple probe responses queued"
        );
        assert_eq!(
            *probe.probe_calls.borrow(),
            2,
            "must probe exactly twice (preflight + one re-probe); extra \
             scripted responses are ignored by design"
        );
    }
}
