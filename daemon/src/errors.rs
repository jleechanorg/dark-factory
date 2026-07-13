#[derive(thiserror::Error, Debug)]
pub enum DaemonError {
    #[error("tool {tool} failed (rc={rc}): {stderr}")]
    Tool {
        tool: String,
        rc: i32,
        stderr: String,
    },
    #[error("parse: {0}")]
    Parse(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("config: {0}")]
    Config(String),
    /// `ao spawn` declined to synchronously create a session and instead
    /// enqueued a deferred `SpawnRequest` (its own internal admission-control
    /// queue, hit when a project's active-session count is at/above AO's
    /// configured cap). AO prints `REQUEST=<id>` instead of `SESSION=<id>`
    /// and exits 0 — no worktree, branch, or process was ever created for
    /// this call. jleechan-5ia2: the daemon used to fall through to
    /// `DaemonError::Parse` ("ao spawn produced no session name"), which is
    /// fatal and crashes the whole daemon process (`main.rs` calls
    /// `std::process::exit(1)` on any non-transient tick error) — confirmed
    /// live via `rust-daemon.err.log` showing 18 systemd restarts in ~15
    /// minutes while `worldarchitect` sat at its 20-session cap. Unlike a
    /// genuine parse failure, this case is provably safe to retry: AO
    /// guarantees no live process exists yet, so there is nothing to leak or
    /// double-spawn by requeuing the bead and trying again next tick.
    #[error("ao spawn deferred to internal queue: {0}")]
    Deferred(String),
    /// The re-roll circuit-breaker's semantic comparator
    /// (`same_underlying_issue` in `reroll.rs`) makes a real LLM call to
    /// judge whether two consecutive rejection reviews describe the same
    /// underlying issue. jleechan-cq8r: an occasional malformed or
    /// unparseable reply from that call used to fall through to
    /// `DaemonError::Parse`, which is fatal and crashes the whole daemon
    /// process via `main.rs`'s `std::process::exit(1)` on any
    /// non-transient tick error -- the EXACT jleechan-5ia2 crash-loop
    /// pattern (see the `Deferred` variant above, PR #197), reintroduced
    /// through this brand-new subprocess-LLM call site. Unlike a genuine
    /// parse bug elsewhere in the daemon, an occasional malformed judge()
    /// reply is an expected, retry-safe condition: the circuit-breaker
    /// check runs before `save_rejection` durably records anything for
    /// this attempt, so nothing is lost or duplicated by backing off and
    /// retrying the comparator call on a later tick.
    #[error("circuit-breaker comparator reply unparseable: {0}")]
    ComparatorUnparseable(String),
    /// jleechan-r56m: `CliSessions::spawn_with_fallback` walks a vendor
    /// fallback chain (e.g. minimax -> claude-code -> agy). Before this fix,
    /// each iteration overwrote a single `last_err` variable, so once every
    /// vendor in the chain had failed, only the LAST vendor's error survived
    /// into the returned `Err`. Live incident: bead jleechan-93ft attempt 3
    /// (2026-07-10T19:51:59Z) fired `REROLL_ADOPTED_REMEDIATION_START`, all
    /// three vendors failed, and `REROLL_ADOPTED_SPAWN_FAILED` /
    /// `PARKED_HUMAN_HELD` telemetry recorded only "ao spawn --agent agy
    /// rc=1 'Agent plugin agy not found'" -- discarding whatever minimax and
    /// claude-code failed with, which misled triage into believing the agy
    /// plugin itself was the root cause instead of merely the last vendor
    /// tried. This variant carries every attempted vendor name paired with
    /// its own specific `DaemonError`, so `Display`/`to_string()` (consumed
    /// verbatim by `reroll.rs`'s `REROLL_ADOPTED_SPAWN_FAILED` telemetry and,
    /// via the `RerollOutcome::Held` reason string, by `tick.rs`'s
    /// `PARKED_HUMAN_HELD` telemetry) always shows the whole chain.
    #[error("all {} fallback vendor(s) failed: {}", .0.len(), format_spawn_attempts(.0))]
    SpawnFallbackExhausted(Vec<(String, DaemonError)>),
}

/// Renders every `(vendor, error)` pair collected by
/// `CliSessions::spawn_with_fallback` into a single semicolon-joined string,
/// e.g. `"minimax: ao spawn --agent minimax rc=1 'auth failed'; claude-code:
/// ao spawn --agent claude-code rc=1 'session cap'; agy: ao spawn --agent agy
/// rc=1 'Agent plugin agy not found'"`. Pulled out as a free function (rather
/// than inlined in the `#[error(...)]` attribute) purely for readability --
/// thiserror supports calling it directly via the `.0` field-access
/// shorthand.
fn format_spawn_attempts(attempts: &[(String, DaemonError)]) -> String {
    attempts
        .iter()
        .map(|(vendor, err)| format!("{vendor}: {err}"))
        .collect::<Vec<_>>()
        .join("; ")
}

impl DaemonError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            DaemonError::Tool { .. }
                | DaemonError::Timeout(_)
                | DaemonError::Deferred(_)
                | DaemonError::ComparatorUnparseable(_)
        ) || matches!(
            self,
            DaemonError::SpawnFallbackExhausted(attempts)
                if attempts.last().is_some_and(|(_, e)| e.is_transient())
        )
    }

    /// True for a bare `Deferred`, or for a `SpawnFallbackExhausted` whose
    /// LAST attempted vendor's own error was itself `Deferred` -- i.e. AO's
    /// own admission-control queue backpressure (session-cap saturation),
    /// not a genuine vendor failure.
    ///
    /// jleechan-r56m regression note: before `spawn_with_fallback` started
    /// wrapping exhausted chains in `SpawnFallbackExhausted`, callers like
    /// `dispatch.rs` matched the literal pattern `DaemonError::Deferred(_)`
    /// to special-case AO backpressure (jleechan-w28n: this case must never
    /// increment `spawn_failure_count`, or sustained cap saturation would
    /// eventually park the whole backlog `HUMAN_HELD` as a mass false
    /// escalation). Once the terminal vendor's `Deferred` got wrapped in
    /// `SpawnFallbackExhausted`, that literal-pattern match went dead for
    /// every real spawn — this accessor restores the special case by
    /// unwrapping to the terminal attempt's own classification. Callers that
    /// used to write `Err(err @ DaemonError::Deferred(_))` should use
    /// `Err(err) if err.is_deferred()` instead, still ahead of the general
    /// `is_transient()` guard (match arm order still matters).
    pub fn is_deferred(&self) -> bool {
        match self {
            DaemonError::Deferred(_) => true,
            DaemonError::SpawnFallbackExhausted(attempts) => {
                attempts.last().is_some_and(|(_, e)| e.is_deferred())
            }
            _ => false,
        }
    }

    /// Detects `br create --external-ref ...` failing because the ref is
    /// already tracked (`br`'s own uniqueness constraint on `external_ref`),
    /// e.g. `Error: Configuration error: External reference 'owner/repo#42'
    /// already exists on issue jleechan-abcd`.
    ///
    /// This is a *write-time* duplicate signal, distinct from (and more
    /// authoritative than) the *read-time* `known_refs.contains(..)`
    /// pre-check in `intake::normalize` / `intake::normalize_labeled_prs`:
    /// `br create`'s own uniqueness check queries the durable store directly
    /// at write time, so it cannot suffer whatever staleness/pagination-skew
    /// affects a preceding bulk `br list` snapshot (jleechan-u4gb — the
    /// pre-check occasionally missed a ref that a concurrent `br create`
    /// correctly rejected as a duplicate seconds later). Treating this error
    /// shape as "already tracked, not a failure" makes the create-bead path
    /// idempotent by construction instead of relying solely on a racy read,
    /// and stops it from killing the whole tick with an exponential backoff
    /// retry loop that can never succeed (the ref will *always* already
    /// exist on retry).
    ///
    /// Returns the existing bead id parsed out of the error message, if the
    /// error matches this specific shape.
    pub fn duplicate_external_ref_bead_id(&self) -> Option<String> {
        let DaemonError::Tool { stderr, .. } = self else {
            return None;
        };
        let marker = "already exists on issue";
        let idx = stderr.find(marker)?;
        let rest = stderr[idx + marker.len()..].trim_start();
        let id = rest
            .split(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .find(|tok| !tok.is_empty())?;
        Some(id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_tool_and_timeout_as_transient() {
        let tool = DaemonError::Tool {
            tool: "gh".to_string(),
            rc: 1,
            stderr: "temporary unavailable".to_string(),
        };
        let timeout = DaemonError::Timeout("ao status timed out".to_string());

        assert!(tool.is_transient());
        assert!(timeout.is_transient());
    }

    #[test]
    fn classifies_config_and_parse_as_fatal() {
        assert!(!DaemonError::Config("missing config".to_string()).is_transient());
        assert!(!DaemonError::Parse("bad json".to_string()).is_transient());
    }

    /// jleechan-5ia2: AO's own internal spawn queue (hit at its active-session
    /// cap) is a retry-safe condition, not a crash-the-daemon condition — see
    /// the `Deferred` variant doc comment for the live-reproduced crash loop
    /// this fixes.
    #[test]
    fn classifies_deferred_as_transient() {
        assert!(DaemonError::Deferred("REQUEST=sq-abc123".to_string()).is_transient());
    }

    #[test]
    fn bare_deferred_is_deferred() {
        assert!(DaemonError::Deferred("REQUEST=sq-abc123".to_string()).is_deferred());
    }

    #[test]
    fn tool_error_is_not_deferred() {
        assert!(!DaemonError::Tool {
            tool: "ao".to_string(),
            rc: 1,
            stderr: "boom".to_string(),
        }
        .is_deferred());
    }

    /// jleechan-r56m regression guard: when `spawn_with_fallback`'s LAST
    /// attempted vendor hit AO's own admission-queue backpressure
    /// (`Deferred`), `is_deferred()` on the aggregated
    /// `SpawnFallbackExhausted` must still report `true` so
    /// `dispatch.rs`'s jleechan-w28n special case (never increment
    /// `spawn_failure_count` for pure backpressure) keeps firing exactly as
    /// it did before this fallback-chain aggregation existed.
    #[test]
    fn spawn_fallback_exhausted_is_deferred_when_last_attempt_is_deferred() {
        let err = DaemonError::SpawnFallbackExhausted(vec![
            (
                "minimax".to_string(),
                DaemonError::Tool {
                    tool: "ao spawn --agent minimax".to_string(),
                    rc: 1,
                    stderr: "auth failure".to_string(),
                },
            ),
            (
                "claude-code".to_string(),
                DaemonError::Deferred("REQUEST=sq-scripted".to_string()),
            ),
        ]);
        assert!(err.is_deferred());
    }

    /// The converse: if the LAST attempt was a genuine (non-Deferred)
    /// failure, `is_deferred()` must be `false` even if an EARLIER attempt
    /// happened to be `Deferred` -- only the terminal outcome matters, same
    /// as pre-fix `last_err` semantics.
    #[test]
    fn spawn_fallback_exhausted_is_not_deferred_when_last_attempt_is_not_deferred() {
        let err = DaemonError::SpawnFallbackExhausted(vec![
            (
                "minimax".to_string(),
                DaemonError::Deferred("REQUEST=sq-scripted".to_string()),
            ),
            (
                "agy".to_string(),
                DaemonError::Tool {
                    tool: "ao spawn --agent agy".to_string(),
                    rc: 1,
                    stderr: "Agent plugin agy not found".to_string(),
                },
            ),
        ]);
        assert!(!err.is_deferred());
    }

    /// jleechan-cq8r: a malformed/unparseable reply from the circuit-breaker
    /// comparator (`same_underlying_issue` in `reroll.rs`) is a retry-safe
    /// condition, not a crash-the-daemon condition -- reproduces the
    /// jleechan-5ia2 crash-loop pattern (PR #197) through a brand-new
    /// subprocess-LLM call site; see the `ComparatorUnparseable` variant
    /// doc comment.
    #[test]
    fn classifies_comparator_unparseable_as_transient() {
        assert!(DaemonError::ComparatorUnparseable(
            "no JSON object found in circuit-breaker comparator reply".to_string()
        )
        .is_transient());
    }

    #[test]
    fn duplicate_external_ref_bead_id_parses_real_br_error() {
        let err = DaemonError::Tool {
            tool: "br".to_string(),
            rc: 7,
            stderr: "Error: Configuration error: External reference 'jleechanorg/worldarchitect.ai#8227' already exists on issue jleechan-vj89\n".to_string(),
        };
        assert_eq!(
            err.duplicate_external_ref_bead_id(),
            Some("jleechan-vj89".to_string())
        );
    }

    #[test]
    fn duplicate_external_ref_bead_id_none_for_unrelated_tool_error() {
        let err = DaemonError::Tool {
            tool: "br".to_string(),
            rc: 1,
            stderr: "some other failure".to_string(),
        };
        assert_eq!(err.duplicate_external_ref_bead_id(), None);
    }

    #[test]
    fn duplicate_external_ref_bead_id_none_for_non_tool_error() {
        let err = DaemonError::Timeout("br list timed out".to_string());
        assert_eq!(err.duplicate_external_ref_bead_id(), None);
    }

    /// jleechan-r56m: `Display`/`to_string()` on `SpawnFallbackExhausted`
    /// must show every attempted vendor's own error, not just one of them --
    /// this is what `reroll.rs`'s `REROLL_ADOPTED_SPAWN_FAILED` telemetry
    /// (`"error": e.to_string()`) and, via the `RerollOutcome::Held` reason
    /// string, `tick.rs`'s `PARKED_HUMAN_HELD` telemetry both consume
    /// verbatim.
    #[test]
    fn spawn_fallback_exhausted_display_includes_every_vendor_error() {
        let err = DaemonError::SpawnFallbackExhausted(vec![
            (
                "minimax".to_string(),
                DaemonError::Tool {
                    tool: "ao spawn --agent minimax".to_string(),
                    rc: 1,
                    stderr: "MINIMAX_MARKER".to_string(),
                },
            ),
            (
                "claude-code".to_string(),
                DaemonError::Tool {
                    tool: "ao spawn --agent claude-code".to_string(),
                    rc: 1,
                    stderr: "CLAUDE_CODE_MARKER".to_string(),
                },
            ),
            (
                "agy".to_string(),
                DaemonError::Tool {
                    tool: "ao spawn --agent agy".to_string(),
                    rc: 1,
                    stderr: "Agent plugin agy not found".to_string(),
                },
            ),
        ]);

        let rendered = err.to_string();
        assert!(rendered.contains("minimax"), "got: {rendered}");
        assert!(rendered.contains("MINIMAX_MARKER"), "got: {rendered}");
        assert!(rendered.contains("claude-code"), "got: {rendered}");
        assert!(rendered.contains("CLAUDE_CODE_MARKER"), "got: {rendered}");
        assert!(rendered.contains("agy"), "got: {rendered}");
        assert!(
            rendered.contains("Agent plugin agy not found"),
            "got: {rendered}"
        );
    }

    /// A `SpawnFallbackExhausted` whose LAST attempt was itself transient
    /// (e.g. the final vendor hit AO's retry-safe `Deferred` admission-queue
    /// case) must remain transient overall, preserving the exact retry
    /// behavior the pre-fix code had when `last_err` alone determined
    /// transience.
    #[test]
    fn spawn_fallback_exhausted_is_transient_when_last_attempt_is_transient() {
        let err = DaemonError::SpawnFallbackExhausted(vec![
            (
                "minimax".to_string(),
                DaemonError::Tool {
                    tool: "ao spawn --agent minimax".to_string(),
                    rc: 1,
                    stderr: "auth failure".to_string(),
                },
            ),
            (
                "claude-code".to_string(),
                DaemonError::Deferred("REQUEST=sq-abc123".to_string()),
            ),
        ]);
        assert!(err.is_transient());
    }

    /// A `SpawnFallbackExhausted` whose LAST attempt is a fatal shape (e.g.
    /// `Config`/`Parse`) must NOT be treated as transient, matching what the
    /// pre-fix `last_err`-only classification would have done.
    #[test]
    fn spawn_fallback_exhausted_is_not_transient_when_last_attempt_is_fatal() {
        let err = DaemonError::SpawnFallbackExhausted(vec![(
            "agy".to_string(),
            DaemonError::Parse("ao spawn --agent agy produced no SESSION= line".to_string()),
        )]);
        assert!(!err.is_transient());
    }
}
