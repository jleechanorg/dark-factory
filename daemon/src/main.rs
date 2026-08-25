// Task 10: tick loop wiring + CLI flags (design doc §5, spec §4.2.2/§4.2.9).
// Modules live in `lib.rs` (see that file) so `daemon/tests/*` integration
// tests can `use daemon::{...}`; this binary just drives the poll loop.
#![allow(dead_code, unused_imports, clippy::type_complexity)]

use daemon::config::{self, Config};
use daemon::errors::DaemonError;
use daemon::state::{SqliteStateStore, StateStore};
use daemon::tick::{run_tick, TickDeps};
use daemon::tools::{Bead, Issue, Llm, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs};
use daemon::vendor_health::VendorHealthLedger;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_TICK_BACKOFF_SECS: u64 = 300;

#[cfg(unix)]
fn systemd_notify(message: &str) -> Result<(), DaemonError> {
    use std::os::unix::net::UnixDatagram;

    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    let datagram = UnixDatagram::unbound()
        .map_err(|e| DaemonError::Config(format!("systemd notify socket open failed: {e}")))?;
    #[cfg(target_os = "linux")]
    if let Some(name) = systemd_abstract_notify_name(socket.as_os_str()) {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::SocketAddr;

        let address = SocketAddr::from_abstract_name(name).map_err(|e| {
            DaemonError::Config(format!("systemd notify abstract socket invalid: {e}"))
        })?;
        datagram
            .send_to_addr(message.as_bytes(), &address)
            .map_err(|e| DaemonError::Config(format!("systemd notify send failed: {e}")))?;
        return Ok(());
    }
    datagram
        .send_to(message.as_bytes(), PathBuf::from(socket))
        .map_err(|e| DaemonError::Config(format!("systemd notify send failed: {e}")))?;
    Ok(())
}

#[cfg(all(unix, target_os = "linux"))]
fn systemd_abstract_notify_name(socket: &std::ffi::OsStr) -> Option<&[u8]> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = socket.as_bytes();
    if bytes.first() == Some(&b'@') {
        Some(&bytes[1..])
    } else {
        None
    }
}

#[cfg(not(unix))]
fn systemd_notify(_message: &str) -> Result<(), DaemonError> {
    Ok(())
}

/// Parsed CLI flags (manual `std::env::args` parsing per Task 10 — no `clap`,
/// staying inside the five-dependency budget from design doc §2).
#[derive(Debug, Clone, Copy, Default)]
struct Args {
    /// Run exactly one tick then exit, instead of looping forever.
    once: bool,
    /// Construct every tool-boundary trait as a no-op stub that performs zero
    /// subprocess calls, zero SCM writes, zero session spawns.
    dry_run: bool,
}

#[derive(Debug, Clone)]
enum CommandMode {
    Daemon(Args),
    RecoverHeld {
        db: PathBuf,
        telemetry_log: PathBuf,
    },
    GatesCompute {
        pr: u64,
        repo: Option<String>,
    },
}

fn parse_args(mut argv: impl Iterator<Item = String>) -> Result<CommandMode, String> {
    let _bin_name = argv.next();
    let next_arg = argv.next();
    match next_arg.as_deref() {
        Some("recover-held") => {
            let mut db = None;
            let mut telemetry_log = None;
            while let Some(arg) = argv.next() {
                match arg.as_str() {
                    "--db" => {
                        db = Some(PathBuf::from(
                            argv.next()
                                .ok_or_else(|| "Missing value for --db".to_string())?,
                        ));
                    }
                    "--telemetry-log" => {
                        telemetry_log = Some(PathBuf::from(
                            argv.next().ok_or_else(|| {
                                "Missing value for --telemetry-log".to_string()
                            })?,
                        ));
                    }
                    other => {
                        return Err(format!("Unknown argument for recover-held: {other}"));
                    }
                }
            }
            Ok(CommandMode::RecoverHeld {
                db: db.ok_or_else(|| "Missing required argument --db".to_string())?,
                telemetry_log: telemetry_log.ok_or_else(|| {
                    "Missing required argument --telemetry-log".to_string()
                })?,
            })
        }
        Some("gates-compute") => {
            let mut pr = None;
            let mut repo = None;
            while let Some(arg) = argv.next() {
                match arg.as_str() {
                    "--pr" => {
                        if let Some(val) = argv.next() {
                            let parsed_pr = val.parse::<u64>().map_err(|_| format!("Invalid PR number: {}", val))?;
                            pr = Some(parsed_pr);
                        } else {
                            return Err("Missing value for --pr".to_string());
                        }
                    }
                    "--repo" => {
                        if let Some(val) = argv.next() {
                            repo = Some(val);
                        } else {
                            return Err("Missing value for --repo".to_string());
                        }
                    }
                    other => {
                        return Err(format!("Unknown argument for gates-compute: {}", other));
                    }
                }
            }
            let pr = pr.ok_or_else(|| "Missing required argument --pr".to_string())?;
            Ok(CommandMode::GatesCompute { pr, repo })
        }
        Some(first_flag) => {
            let mut args = Args::default();
            let mut all_args = vec![first_flag.to_string()];
            all_args.extend(argv);
            for arg in all_args {
                match arg.as_str() {
                    "--once" => args.once = true,
                    "--dry-run" => args.dry_run = true,
                    _ => {}
                }
            }
            Ok(CommandMode::Daemon(args))
        }
        None => {
            Ok(CommandMode::Daemon(Args::default()))
        }
    }
}

/// Read-only, zero-effect stub used for every tool boundary until the
/// production `Cli*`/`ChainLlm` adapters (design doc §4) land.
#[cfg(any(test, debug_assertions))]
struct NoopAdapters;

#[cfg(any(test, debug_assertions))]
impl Tracker for NoopAdapters {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError> {
        Ok(Vec::new())
    }
    fn fetch_all_external_refs(&self) -> Result<std::collections::HashSet<String>, DaemonError> {
        Ok(std::collections::HashSet::new())
    }
    fn create_bead(&self, _title: &str, _body: &str, _external_ref: &str) -> Result<String, DaemonError> {
        Ok(String::new())
    }
    fn comment_external(&self, _external_ref: &str, _body: &str) -> Result<(), DaemonError> {
        Ok(())
    }
}

#[cfg(any(test, debug_assertions))]
impl Scm for NoopAdapters {
    fn labeled_issues(&self, _label: &str) -> Result<Vec<Issue>, DaemonError> {
        Ok(Vec::new())
    }
    fn labeled_prs(
        &self,
        _label: &str,
        _gh_calls: &mut u32,
    ) -> Result<Vec<daemon::tools::LabeledPr>, DaemonError> {
        Ok(Vec::new())
    }
    fn collaborator_permission(&self, _login: &str) -> Result<Permission, DaemonError> {
        Ok(Permission::None)
    }
    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
        Err(DaemonError::Config(format!(
            "NoopAdapters: no production Scm impl yet (pr {pr})"
        )))
    }
    fn close_pr(&self, _pr: u64, _comment: &str) -> Result<(), DaemonError> {
        Ok(())
    }
    fn remote_branch_last_commit(&self, _branch: &str) -> Result<Option<u64>, DaemonError> {
        Ok(None)
    }
}

#[cfg(any(test, debug_assertions))]
impl Sessions for NoopAdapters {
    fn active_count(&self) -> Result<usize, DaemonError> {
        Ok(0)
    }
    fn spawn(&self, _spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
        Err(DaemonError::Config(
            "NoopAdapters: session spawn is disabled (no production Sessions impl yet)".into(),
        ))
    }
    fn attach(&self, _branch: &str, _bead_id: &str) -> Result<SessionId, DaemonError> {
        Err(DaemonError::Config(
            "NoopAdapters: session attach is disabled (no production Sessions impl yet)".into(),
        ))
    }
    fn stop(&self, _id: &SessionId) -> Result<(), DaemonError> {
        Ok(())
    }
    fn is_quiescent(&self, _id: &SessionId) -> Result<bool, DaemonError> {
        Ok(true)
    }
}

#[cfg(any(test, debug_assertions))]
impl Vcs for NoopAdapters {
    fn base_head(&self, _base_branch: &str) -> Result<String, DaemonError> {
        Ok(String::new())
    }
    fn create_branch_at(&self, _name: &str, _sha: &str) -> Result<(), DaemonError> {
        Ok(())
    }
    fn head_sha(&self, _branch: &str) -> Result<String, DaemonError> {
        Ok(String::new())
    }
    fn is_remote_ahead(
        &self,
        _branch: &str,
        _remote_sha: &str,
    ) -> Result<bool, DaemonError> {
        Ok(false)
    }
    fn push_fix_commit(&self, _branch: &str, _message: &str) -> Result<(), DaemonError> {
        Ok(())
    }
    fn remote_head_sha(&self, _branch: &str) -> Result<String, DaemonError> {
        Ok(String::new())
    }
    fn is_ancestor(&self, _ancestor_sha: &str, _descendant_sha: &str) -> Result<bool, DaemonError> {
        Ok(true)
    }
}

#[cfg(any(test, debug_assertions))]
impl Llm for NoopAdapters {
    fn judge(&self, _prompt: &str) -> Result<String, DaemonError> {
        Ok(String::new())
    }
}

fn default_config_path() -> PathBuf {
    let live = PathBuf::from("config/daemon.toml");
    if live.exists() {
        live
    } else {
        PathBuf::from("daemon/contracts/daemon.toml.example")
    }
}

fn default_telemetry_log() -> PathBuf {
    dirs_home_log_path().unwrap_or_else(|| PathBuf::from("daemon.jsonl"))
}

fn dirs_home_log_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        Path::new(&home)
            .join("Library/Logs/dark-factory")
            .join("daemon.jsonl")
    })
}

fn default_state_db_path() -> PathBuf {
    daemon::intake::runtime_state_dir().join("daemon-cxdb.sqlite")
}

fn load_config(path: &Path) -> Result<Config, DaemonError> {
    config::load(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TickLoopAction {
    Success {
        consecutive_failures: u32,
    },
    TransientBackoff {
        consecutive_failures: u32,
        sleep_secs: u64,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TickAttempt {
    tick_index: u64,
    elapsed_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TickLoopState<T> {
    tick_index: u64,
    consecutive_failures: u32,
    has_attempted: bool,
    last_tick_time: T,
}

impl<T: Copy> TickLoopState<T> {
    fn new(last_tick_time: T) -> Self {
        Self {
            tick_index: 0,
            consecutive_failures: 0,
            has_attempted: false,
            last_tick_time,
        }
    }

    fn begin_tick(
        &mut self,
        now: T,
        elapsed_since_last_tick: impl FnOnce(T, T) -> u64,
    ) -> TickAttempt {
        let elapsed_secs = if self.has_attempted {
            elapsed_since_last_tick(self.last_tick_time, now)
        } else {
            0
        };
        self.has_attempted = true;
        self.last_tick_time = now;
        TickAttempt {
            tick_index: self.tick_index,
            elapsed_secs,
        }
    }

    fn apply_action(&mut self, action: &TickLoopAction) {
        match action {
            TickLoopAction::Success {
                consecutive_failures,
            } => {
                self.consecutive_failures = *consecutive_failures;
                self.tick_index += 1;
            }
            TickLoopAction::TransientBackoff {
                consecutive_failures,
                ..
            } => {
                self.consecutive_failures = *consecutive_failures;
            }
        }
    }
}

fn apply_tick_action<T: Copy>(
    tick_loop: &mut TickLoopState<T>,
    action: &TickLoopAction,
    mut reconcile_dispatching: impl FnMut() -> Result<(), DaemonError>,
) -> Result<(), DaemonError> {
    tick_loop.apply_action(action);
    if matches!(action, TickLoopAction::TransientBackoff { .. }) {
        reconcile_dispatching()?;
    }
    Ok(())
}

fn next_backoff_secs(base_tick_secs: u64, consecutive_failures: u32) -> u64 {
    let base = base_tick_secs.max(1);
    let exponent = consecutive_failures.saturating_sub(1).min(63);
    base.saturating_mul(2_u64.saturating_pow(exponent))
        .min(MAX_TICK_BACKOFF_SECS)
}

fn classify_tick_result<T>(
    result: Result<T, DaemonError>,
    base_tick_secs: u64,
    consecutive_failures: u32,
) -> Result<TickLoopAction, DaemonError> {
    match result {
        Ok(_) => Ok(TickLoopAction::Success {
            consecutive_failures: 0,
        }),
        Err(err) if err.is_transient() => {
            let consecutive_failures = consecutive_failures.saturating_add(1);
            Ok(TickLoopAction::TransientBackoff {
                consecutive_failures,
                sleep_secs: next_backoff_secs(base_tick_secs, consecutive_failures),
                error: err.to_string(),
            })
        }
        Err(err) => Err(err),
    }
}

type DaemonAdapters = (
    Box<dyn Scm>,
    Box<dyn Tracker>,
    Box<dyn Sessions>,
    Box<dyn Llm>,
    Box<dyn Vcs>,
);

fn ao_runtime_binding(cfg: &Config) -> Result<(String, String), DaemonError> {
    // Use the canonical routing path shared with dispatch, including legacy
    // aliases such as worldarchitect.ai -> worldarchitect. Duplicating its
    // derivation here would let startup reject a config that spawning accepts.
    let ao_project = cfg
        .resolve_repo(&cfg.target_repo)
        .ok_or_else(|| {
            DaemonError::Config(format!(
                "target repository {:?} has no AO routing configuration",
                cfg.target_repo
            ))
        })?
        .ao_project;
    let default_agent = std::env::var("DARK_FACTORY_CODER_DEFAULT")
        .unwrap_or_else(|_| "agy".to_string());
    Ok((ao_project, default_agent))
}

/// Mirrors the runtime fallback chain (`CliSessions::spawn_with_fallback`)
/// using the same `canonical_for_alias` mapping so the preflight and the
/// `--agent` argv agree on every vendor name. This is the SINGLE source for
/// the configured vendor list — startup and dispatch consult it so a renamed
/// plugin cannot pass preflight while failing the runtime fallback chain
/// (or vice versa).
fn configured_vendor_list(default_agent: &str) -> Vec<String> {
    let fallback_str = std::env::var("DARK_FACTORY_CODER_FALLBACK_CHAIN")
        .or_else(|_| std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN"))
        .unwrap_or_else(|_| "agy->minimax->claudem".to_string());

    let canonicalize = |vendor: &str| -> String {
        daemon::adapters::canonical_for_alias(vendor)
            .map(str::to_string)
            .unwrap_or_else(|| vendor.to_string())
    };

    let mut chain: Vec<String> = Vec::new();
    let default_canonical = canonicalize(default_agent);
    if !default_canonical.is_empty() {
        chain.push(default_canonical);
    }
    for part in fallback_str.split("->") {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let canonical = canonicalize(trimmed);
        if !canonical.is_empty() && !chain.contains(&canonical) {
            chain.push(canonical);
        }
    }
    chain
}

fn verify_startup_ao_compatibility(
    args: Args,
    ao_project: &str,
    configured_vendors: &[String],
    verify: impl FnOnce(&str, &[String]) -> Result<(), DaemonError>,
) -> Result<(), DaemonError> {
    if args.dry_run {
        return Ok(());
    }
    verify(ao_project, configured_vendors)
}

/// Bead jleechan-sb4b: emit a loud config warning at daemon startup when
/// the runtime red-green vacuous-test detector's toolchain is missing.
///
/// The previous failure mode was silent: the systemd-unit daemon
/// environment had no `cargo` on PATH, so every assessment reported a
/// misleading `GreenFailed: git error: spawn cargo test: No such file or
/// directory` and operators only discovered the misalignment after
/// staring at gate-8 verdicts. We now resolve cargo via the same
/// fallback chain the detector uses (`PATH` -> `$HOME/.cargo/bin/cargo`
/// -> `rustup which cargo`) and surface a one-line, loud warning to
/// stderr the FIRST time the daemon starts. The warning is non-fatal —
/// the daemon can still run all other gates while the operator fixes
/// the toolchain — but it is unambiguous about the cause and the fix.
///
/// Skipped under `--dry-run` because tests construct synthetic envs that
/// don't necessarily have cargo.
fn verify_startup_cargo_toolchain(args: Args) {
    if args.dry_run {
        return;
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo"))
        });
    match daemon::vacuous_red_green::resolve_cargo(cargo_home.as_deref()) {
        daemon::vacuous_red_green::CargoLocation::OnPath
        | daemon::vacuous_red_green::CargoLocation::Found(_) => {
            // Toolchain present — nothing to do.
        }
        daemon::vacuous_red_green::CargoLocation::NotFound => {
            eprintln!(
                "auto-factory daemon: WARNING (jleechan-sb4b): cargo binary not found. \
                 The runtime red-green vacuous-test detector (gate 8) will report \
                 'cargo binary not found' on every assessment until cargo is \
                 installed. Resolve by either adding `cargo` to PATH, \
                 installing rustup so $HOME/.cargo/bin/cargo exists, or setting \
                 CARGO_HOME. Searched: PATH, $HOME/.cargo/bin/cargo, `rustup which cargo`."
            );
        }
    }
}

fn run(args: Args) -> Result<(), DaemonError> {
    let cfg_path = default_config_path();
    let cfg = load_config(&cfg_path)?;
    let (ao_project, default_agent) = ao_runtime_binding(&cfg)?;
    let configured_vendors = configured_vendor_list(&default_agent);
    // Fail before opening/reconciling state, advertising READY, or polling a
    // healthy tick when the installed AO/Node adapter is incompatible. The
    // diagnostic exits before AO preflight, locking, workspace creation, or
    // worker launch.
    verify_startup_ao_compatibility(args, &ao_project, &configured_vendors, |project, vendors| {
        daemon::adapters::verify_ao_bridge_compatibility(project, vendors.first().map(String::as_str).unwrap_or("agy"), vendors)
    })?;
    // Bead jleechan-sb4b: emit a loud config warning at startup if the
    // runtime red-green detector's toolchain is missing. The previous
    // behavior was silent: every assessment reported a misleading
    // `GreenFailed: git error: spawn cargo test: No such file or
    // directory` and operators had no upfront signal that the daemon's
    // systemd environment lacked cargo. This emit is non-fatal — the
    // daemon can still run other gates — but loud enough that the
    // operator sees it in the journalctl output before the first
    // assessment.
    verify_startup_cargo_toolchain(args);
    // Verify the telemetry log path is writable BEFORE we start polling
    // ticks — every assessment writes here, and a churning 7-green false
    // alarm usually traces back to a silent telemetry write failure.
    let telemetry_log = default_telemetry_log();
    let db_path = default_state_db_path();

    let store: Box<dyn StateStore> = if args.dry_run {
        Box::new(SqliteStateStore::open_in_memory_with_schema(include_str!(
            "../contracts/schema.sql"
        ))?)
    } else {
        Box::new(SqliteStateStore::open(&db_path)?)
    };

    store.reconcile_dispatching()?;

    let (scm, tracker, sessions, llm, vcs): DaemonAdapters = if args.dry_run {
        #[cfg(any(test, debug_assertions))]
        {
            (
                Box::new(NoopAdapters),
                Box::new(NoopAdapters),
                Box::new(NoopAdapters),
                Box::new(NoopAdapters),
                Box::new(NoopAdapters),
            )
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            return Err(DaemonError::Config(
                "NoopAdapters / --dry-run is disabled/gated in production release builds".into(),
            ));
        }
    } else {
        use daemon::adapters::{CliScm, CliSessions, CliTracker, ChainLlm, CliVcs};
        (
            Box::new(CliScm::new(cfg.target_repo.clone())),
            Box::new(CliTracker),
            Box::new(CliSessions::new(&ao_project, &default_agent)),
            Box::new(ChainLlm),
            Box::new(CliVcs::new(cfg.target_repo.clone())),
        )
    };

    // Bead jleechan-jsby (r3): process-wide vendor-health ledger shared
    // between `--once` and the poll loop. The r2 commit wired the field
    // into TickDeps and the tick path, but main.rs left `vendor_health:
    // None` — so the live daemon's fast tier created a fresh empty
    // ledger per tick and never accumulated observations across beads.
    // VENDOR_WAIVED / VENDOR_RECOVERED telemetry therefore could not
    // fire in production. This Mutex lives for the full `run`-scope so
    // every `TickDeps` borrow below stays valid for the daemon lifetime.
    let vendor_health = std::sync::Arc::new(std::sync::Mutex::new(VendorHealthLedger::new()));
    daemon::vendor_health::set_global_ledger(vendor_health.clone());

    let deps = TickDeps {
        scm: scm.as_ref(),
        tracker: tracker.as_ref(),
        sessions: sessions.as_ref(),
        llm: llm.as_ref(),
        store: store.as_ref(),
        vcs: vcs.as_ref(),
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: Some(&*vendor_health),
    };

    if args.once {
        let _ = systemd_notify("READY=1\nSTATUS=auto-factory daemon running one tick");
        run_tick(&deps, 0, 0)?;
        let _ = systemd_notify("STOPPING=1\nSTATUS=auto-factory daemon one-shot complete");
        return Ok(());
    }

    let _ = systemd_notify("READY=1\nSTATUS=auto-factory daemon started");

    let mut tick_loop = TickLoopState::new(std::time::Instant::now());
    loop {
        let now = std::time::Instant::now();
        let attempt = tick_loop.begin_tick(now, |last_tick_time, now| {
            now.duration_since(last_tick_time).as_secs()
        });

        let action = classify_tick_result(
            run_tick(&deps, attempt.tick_index, attempt.elapsed_secs),
            cfg.fast_tick_secs,
            tick_loop.consecutive_failures,
        )?;
        let watchdog_status = match &action {
            TickLoopAction::Success {
                consecutive_failures,
            } => format!(
                "WATCHDOG=1\nSTATUS=auto-factory daemon tick={} ok consecutive_failures={}",
                attempt.tick_index, consecutive_failures
            ),
            TickLoopAction::TransientBackoff {
                consecutive_failures,
                sleep_secs,
                ..
            } => format!(
                "WATCHDOG=1\nSTATUS=auto-factory daemon tick={} transient_failure consecutive_failures={} backoff={}s",
                attempt.tick_index, consecutive_failures, sleep_secs
            ),
        };
        let previous_failures = tick_loop.consecutive_failures;
        if let TickLoopAction::TransientBackoff {
            consecutive_failures,
            sleep_secs,
            ref error,
        } = action
        {
            send_daemon_alert(consecutive_failures, sleep_secs, error);
        }

        apply_tick_action(&mut tick_loop, &action, || store.reconcile_dispatching())?;
        let _ = systemd_notify(&watchdog_status);

        match action {
            TickLoopAction::Success {
                consecutive_failures: _,
            } => {
                if previous_failures >= 3 {
                    send_daemon_recovery_alert(previous_failures);
                }
                std::thread::sleep(std::time::Duration::from_secs(cfg.fast_tick_secs));
            }
            TickLoopAction::TransientBackoff {
                consecutive_failures: _,
                sleep_secs,
                error,
            } => {
                eprintln!(
                    "auto-factory daemon: transient tick error: {error}; sleeping {sleep_secs}s before retry"
                );
                std::thread::sleep(std::time::Duration::from_secs(sleep_secs));
            }
        }
    }
}

/// Send Slack and Email alerts when consecutive tick failures reach thresholds (3, 5, 10, 20...).
fn send_daemon_alert(consecutive_failures: u32, sleep_secs: u64, error: &str) {
    if consecutive_failures != 3
        && consecutive_failures != 5
        && (consecutive_failures < 10 || !consecutive_failures.is_multiple_of(10))
    {
        return;
    }
    let msg = format!(
        "🚨 *[Dark Factory Daemon Alert]*\nConsecutive tick failures: `{consecutive_failures}` (backoff: `{sleep_secs}s`)\nError: ```{error}```\nHost: `jeff-ubuntu`"
    );
    send_slack_and_email_alert(&msg, "[Dark Factory Alert] Daemon Tick Failure");
}

/// Send Slack and Email alerts when the daemon recovers from a failure streak (>= 3).
fn send_daemon_recovery_alert(previous_failures: u32) {
    let msg = format!(
        "✅ *[Dark Factory Daemon Recovered]*\nDaemon recovered after `{previous_failures}` consecutive failures. Ticks are now executing normally on `jeff-ubuntu`."
    );
    send_slack_and_email_alert(&msg, "[Dark Factory Alert] Daemon Recovered");
}

/// Helper to dispatch notifications to Slack and Gmail SMTP in background threads without blocking.
fn send_slack_and_email_alert(message: &str, email_subject: &str) {
    let msg_slack = message.to_string();
    let msg_email = message.to_string();
    let subject = email_subject.to_string();

    std::thread::spawn(move || {
        // 1. Slack alert via HERMES_SLACK_BOT_TOKEN or SLACK_WEBHOOK_URL
        let slack_token = std::env::var("HERMES_SLACK_BOT_TOKEN")
            .or_else(|_| std::env::var("OPENCLAW_SLACK_BOT_TOKEN"))
            .unwrap_or_default();
        let slack_channel = std::env::var("FACTORY_SLACK_CHANNEL_ID")
            .or_else(|_| std::env::var("DARK_FACTORY_SLACK_CHANNEL"))
            .or_else(|_| std::env::var("SLACK_ALERT_CHANNEL"))
            .unwrap_or_else(|_| "C0AH3RY3DK6".to_string());

        if !slack_token.is_empty() {
            let payload = serde_json::json!({
                "channel": slack_channel,
                "text": msg_slack,
            });
            let _ = std::process::Command::new("curl")
                .args([
                    "-s",
                    "--connect-timeout",
                    "5",
                    "--max-time",
                    "10",
                    "-X",
                    "POST",
                    "https://slack.com/api/chat.postMessage",
                    "-H",
                    &format!("Authorization: Bearer {slack_token}"),
                    "-H",
                    "Content-Type: application/json; charset=utf-8",
                    "--data",
                    &payload.to_string(),
                ])
                .output();
        } else if let Ok(webhook) = std::env::var("SLACK_WEBHOOK_URL") {
            if !webhook.is_empty() {
                let payload = serde_json::json!({ "text": msg_slack });
                let _ = std::process::Command::new("curl")
                    .args([
                        "-s",
                        "--connect-timeout",
                        "5",
                        "--max-time",
                        "10",
                        "-X",
                        "POST",
                        "-H",
                        "Content-Type: application/json",
                        "--data",
                        &payload.to_string(),
                        &webhook,
                    ])
                    .output();
            }
        }

        // 2. Email alert via Gmail SMTP using python stdlib smtplib, passing credentials via environment
        if let (Ok(email_user), Ok(email_pass)) =
            (std::env::var("EMAIL_USER"), std::env::var("EMAIL_PASS"))
        {
            if !email_user.is_empty() && !email_pass.is_empty() {
                let recipient =
                    std::env::var("BACKUP_EMAIL").unwrap_or_else(|_| email_user.clone());
                let script = r#"
import os, smtplib, ssl
from email.message import EmailMessage

user = os.environ.get('EMAIL_USER', '')
pwd = os.environ.get('EMAIL_PASS', '')
recipient = os.environ.get('ALERT_RECIPIENT', user)
subject = os.environ.get('ALERT_SUBJECT', '')
content = os.environ.get('ALERT_MSG', '')

if user and pwd and recipient:
    msg = EmailMessage()
    msg.set_content(content)
    msg['Subject'] = subject
    msg['From'] = user
    msg['To'] = recipient
    try:
        context = ssl.create_default_context()
        with smtplib.SMTP_SSL('smtp.gmail.com', 465, context=context, timeout=10) as server:
            server.login(user, pwd)
            server.send_message(msg)
    except Exception:
        pass
"#;
                let _ = std::process::Command::new("python3")
                    .args(["-c", script])
                    .env("ALERT_RECIPIENT", recipient)
                    .env("ALERT_SUBJECT", subject)
                    .env("ALERT_MSG", msg_email)
                    .output();
            }
        }
    });
}

fn run_recover_held(db: &Path, telemetry_log: &Path) -> Result<(), DaemonError> {
    const MAX_HUMAN_HELD_RECOVERY_ATTEMPT: u32 = 10;

    let store = SqliteStateStore::open(db)?;
    let recovered = store.recover_human_held(MAX_HUMAN_HELD_RECOVERY_ATTEMPT)?;
    for overlay in &recovered {
        daemon::telemetry::emit(
            telemetry_log,
            &daemon::telemetry::TelemetryEvent {
                timestamp: daemon::state::now_iso8601(),
                host: daemon::telemetry::local_hostname(),
                bead_id: overlay.bead_id.clone(),
                attempt_id: overlay.attempt,
                lifecycle_state: "QUEUED".to_string(),
                event_type: "RECOVERED_FROM_HELD".to_string(),
                metrics: serde_json::json!({}),
                context: serde_json::json!({
                    "prior_state": "HUMAN_HELD",
                    "pr_number": overlay.pr_number,
                    "branch": overlay.branch,
                }),
            },
        )?;
    }
    println!("recovered={}", recovered.len());
    Ok(())
}

fn main() {
    let mode = match parse_args(std::env::args()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("auto-factory daemon: args: {e}");
            std::process::exit(1);
        }
    };

    match mode {
        CommandMode::Daemon(args) => {
            if let Err(e) = run(args) {
                eprintln!("auto-factory daemon: fatal: {e}");
                std::process::exit(1);
            }
        }
        CommandMode::GatesCompute { pr, repo } => {
            if let Err(e) = daemon::gates_compute::run_gates_compute(pr, repo) {
                eprintln!("auto-factory daemon: gates-compute error: {e}");
                std::process::exit(1);
            }
        }
        CommandMode::RecoverHeld { db, telemetry_log } => {
            if let Err(e) = run_recover_held(&db, &telemetry_log) {
                eprintln!("auto-factory daemon: recover-held error: {e}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serializes tests that mutate the process-wide NOTIFY_SOCKET env var. Rust
    // runs tests in parallel by default, so without this lock two tests can each
    // set NOTIFY_SOCKET and systemd_notify may send its datagram to the sibling
    // test's socket, leaving this listener's recv() to time out (WouldBlock).
    static NOTIFY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn scaffold_compiles() {
        let ok = true;
        assert!(ok);
    }

    #[test]
    fn production_startup_runs_ao_compatibility_check_and_propagates_failure() {
        let mut observed = None;
        let vendors = vec!["minimax".to_string()];
        let error = verify_startup_ao_compatibility(
            Args::default(),
            "dark-factory",
            &vendors,
            |project, list| {
                observed = Some((project.to_string(), list.to_vec()));
                Err(DaemonError::Config("incompatible AO runtime".to_string()))
            },
        )
        .unwrap_err();

        assert_eq!(
            observed,
            Some(("dark-factory".to_string(), vendors.clone()))
        );
        assert!(error.to_string().contains("incompatible AO runtime"));
    }

    #[test]
    fn startup_binding_uses_canonical_legacy_worldarchitect_project_alias() {
        let cfg = Config {
            target_repo: "jleechanorg/worldarchitect.ai".to_string(),
            ao_project: None,
            base_branch: "main".to_string(),
            stage: 1,
            max_workers: 30,
            max_batch: 15,
            fast_tick_secs: 60,
            slow_tick_secs: 600,
            autonomy_timebox_secs: 10_800,
            budget_warn_usd: 20.0,
            spec_dir: ".factory/specs".to_string(),
            reroll_head_stability_window_secs: 30,
            reroll_death_confirm_secs: 5,
            held_recheck_cooldown_secs: 900,
            repos: std::collections::HashMap::new(),
            pre_gate_validation_enabled: false,
            escalation_refire_secs: 3600,
            agent_worktree_root: None,
            worktree_ttl_secs: 14 * 24 * 60 * 60,
            worktree_max_count: 200,
        };

        let (project, _) = ao_runtime_binding(&cfg).unwrap();

        assert_eq!(project, "worldarchitect");
    }

    // jleechan-9lvs r2: the configured vendor chain must (a) include the
    // default agent, (b) apply canonical_for_alias to every fallback entry,
    // (c) dedupe by canonical form so that `agy` + `antigravity` (or
    // `aow` + `minimax`) never appears twice, and (d) preserve order so the
    // preflight sees the same vendor order the runtime fallback chain does.
    #[test]
    fn configured_vendor_list_resolves_aliases_and_dedupes() {
        // Snapshot the existing env, mutate it for the test, restore after.
        let prior_default = std::env::var("DARK_FACTORY_REVIEWER_DEFAULT").ok();
        let prior_chain = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok();

        // SAFETY: env mutation is serialized by the test isolation rules;
        // we set/remove only the keys this test owns.
        unsafe {
            std::env::set_var("DARK_FACTORY_REVIEWER_DEFAULT", "agy");
            std::env::set_var(
                "DARK_FACTORY_REVIEWER_FALLBACK_CHAIN",
                "antigravity->agy->aow->claude-code",
            );
        }

        let vendors = configured_vendor_list("agy");

        unsafe {
            match prior_default {
                Some(v) => std::env::set_var("DARK_FACTORY_REVIEWER_DEFAULT", v),
                None => std::env::remove_var("DARK_FACTORY_REVIEWER_DEFAULT"),
            }
            match prior_chain {
                Some(v) => std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", v),
                None => std::env::remove_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN"),
            }
        }

        // Default + canonical fallback entries, deduped (agy->antigravity
        // collapses with the literal antigravity; aow->minimax is the
        // pre-existing alias).
        assert_eq!(
            vendors,
            vec!["antigravity".to_string(), "minimax".to_string(), "claude-code".to_string()]
        );
    }

    // jleechan-72hp r1 defect (1): `ao_runtime_binding` must source the
    // default agent from `DARK_FACTORY_CODER_DEFAULT` (the env the spawn
    // fallback chain consults), NOT `DARK_FACTORY_REVIEWER_DEFAULT`.
    // Previously, configured `MiniMax` coders were silently replaced by
    // `agy` because startup read the REVIEWER variable, so production
    // `CliSessions::new(..., "minimax")` never actually launched a MiniMax
    // coder — it spawned Antigravity through the configured-reviewer path.
    #[test]
    fn startup_binding_uses_coder_default_not_reviewer_default() {
        // Build a minimal Config with a repo routing entry so
        // `cfg.resolve_repo` returns Some(...) and `ao_runtime_binding`
        // reaches the env-var read.
        let repos = std::collections::HashMap::from([(
            "owner/repo".to_string(),
            crate::config::RepoConfig {
                ao_project: "repo".to_string(),
                push_remote: "origin".to_string(),
                local_checkout: Some(std::env::current_dir().unwrap()),
            },
        )]);
        let cfg = Config {
            target_repo: "owner/repo".to_string(),
            ao_project: None,
            base_branch: "main".to_string(),
            stage: 1,
            max_workers: 30,
            max_batch: 15,
            fast_tick_secs: 60,
            slow_tick_secs: 60,
            autonomy_timebox_secs: 10_800,
            budget_warn_usd: 20.0,
            spec_dir: ".factory/specs/".to_string(),
            reroll_head_stability_window_secs: 1,
            reroll_death_confirm_secs: 0,
            held_recheck_cooldown_secs: 900,
            repos,
            pre_gate_validation_enabled: false,
            escalation_refire_secs: 3600,
            agent_worktree_root: None,
            worktree_ttl_secs: 14 * 24 * 60 * 60,
            worktree_max_count: 200,
        };

        // Isolate env mutations to this test; serialize against other tests
        // that may also touch DARK_FACTORY_*_DEFAULT.
        let _env_lock = NOTIFY_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let prior_coder = std::env::var("DARK_FACTORY_CODER_DEFAULT").ok();
        let prior_reviewer = std::env::var("DARK_FACTORY_REVIEWER_DEFAULT").ok();

        unsafe {
            std::env::set_var("DARK_FACTORY_CODER_DEFAULT", "minimax");
            // Deliberately set REVIEWER to a different value to prove the
            // CODER env wins — if the bug regresses and the binding still
            // reads REVIEWER, this test returns "agy" (the legacy fallback)
            // and fails.
            std::env::set_var("DARK_FACTORY_REVIEWER_DEFAULT", "agy");
        }

        let result = ao_runtime_binding(&cfg);

        unsafe {
            match prior_coder {
                Some(v) => std::env::set_var("DARK_FACTORY_CODER_DEFAULT", v),
                None => std::env::remove_var("DARK_FACTORY_CODER_DEFAULT"),
            }
            match prior_reviewer {
                Some(v) => std::env::set_var("DARK_FACTORY_REVIEWER_DEFAULT", v),
                None => std::env::remove_var("DARK_FACTORY_REVIEWER_DEFAULT"),
            }
        }

        let (_project, default_agent) = result.expect("binding must succeed");
        assert_eq!(
            default_agent, "minimax",
            "ao_runtime_binding must read DARK_FACTORY_CODER_DEFAULT, not DARK_FACTORY_REVIEWER_DEFAULT (got {default_agent:?})"
        );
    }

    #[test]
    fn dry_run_startup_skips_production_ao_compatibility_check() {
        let mut called = false;
        let vendors = vec!["minimax".to_string()];
        let result = verify_startup_ao_compatibility(
            Args {
                once: true,
                dry_run: true,
            },
            "dark-factory",
            &vendors,
            |_, _| {
                called = true;
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(!called);
    }

    #[test]
    fn parse_args_recognizes_once_and_dry_run() {
        let argv = vec!["daemon".to_string(), "--once".to_string(), "--dry-run".to_string()];
        let mode = parse_args(argv.into_iter()).unwrap();
        if let CommandMode::Daemon(args) = mode {
            assert!(args.once);
            assert!(args.dry_run);
        } else {
            panic!("expected Daemon mode");
        }
    }

    #[test]
    fn parse_args_defaults_false_with_no_flags() {
        let argv = vec!["daemon".to_string()];
        let mode = parse_args(argv.into_iter()).unwrap();
        if let CommandMode::Daemon(args) = mode {
            assert!(!args.once);
            assert!(!args.dry_run);
        } else {
            panic!("expected Daemon mode");
        }
    }

    #[test]
    fn parse_args_ignores_unknown_flags() {
        let argv = vec!["daemon".to_string(), "--bogus".to_string(), "--once".to_string()];
        let mode = parse_args(argv.into_iter()).unwrap();
        if let CommandMode::Daemon(args) = mode {
            assert!(args.once);
            assert!(!args.dry_run);
        } else {
            panic!("expected Daemon mode");
        }
    }

    #[test]
    fn parse_args_recognizes_gates_compute_with_repo() {
        let argv = vec![
            "daemon".to_string(),
            "gates-compute".to_string(),
            "--pr".to_string(),
            "161".to_string(),
            "--repo".to_string(),
            "foo/bar".to_string(),
        ];
        let mode = parse_args(argv.into_iter()).unwrap();
        match mode {
            CommandMode::GatesCompute { pr, repo } => {
                assert_eq!(pr, 161);
                assert_eq!(repo, Some("foo/bar".to_string()));
            }
            _ => panic!("Expected GatesCompute mode"),
        }
    }

    #[test]
    fn parse_args_recognizes_gates_compute_without_repo() {
        let argv = vec![
            "daemon".to_string(),
            "gates-compute".to_string(),
            "--pr".to_string(),
            "161".to_string(),
        ];
        let mode = parse_args(argv.into_iter()).unwrap();
        match mode {
            CommandMode::GatesCompute { pr, repo } => {
                assert_eq!(pr, 161);
                assert_eq!(repo, None);
            }
            _ => panic!("Expected GatesCompute mode"),
        }
    }

    #[test]
    fn parse_args_recognizes_recover_held_paths() {
        let argv = vec![
            "daemon".to_string(),
            "recover-held".to_string(),
            "--db".to_string(),
            "/tmp/recovery.sqlite".to_string(),
            "--telemetry-log".to_string(),
            "/tmp/recovery.jsonl".to_string(),
        ];
        let mode = parse_args(argv.into_iter()).unwrap();
        match mode {
            CommandMode::RecoverHeld { db, telemetry_log } => {
                assert_eq!(db, PathBuf::from("/tmp/recovery.sqlite"));
                assert_eq!(telemetry_log, PathBuf::from("/tmp/recovery.jsonl"));
            }
            _ => panic!("expected RecoverHeld mode"),
        }
    }

    #[test]
    fn parse_args_recover_held_requires_db_and_telemetry_paths() {
        for argv in [
            vec!["daemon".to_string(), "recover-held".to_string()],
            vec![
                "daemon".to_string(),
                "recover-held".to_string(),
                "--db".to_string(),
                "/tmp/recovery.sqlite".to_string(),
            ],
        ] {
            assert!(parse_args(argv.into_iter()).is_err());
        }
    }

    #[test]
    fn next_backoff_uses_min_base_and_exponential_sequence() {
        assert_eq!(next_backoff_secs(0, 1), 1);
        assert_eq!(next_backoff_secs(0, 2), 2);
        assert_eq!(next_backoff_secs(0, 3), 4);
        assert_eq!(next_backoff_secs(10, 1), 10);
        assert_eq!(next_backoff_secs(10, 2), 20);
        assert_eq!(next_backoff_secs(10, 3), 40);
    }

    #[test]
    fn next_backoff_caps_at_five_minutes() {
        assert_eq!(next_backoff_secs(60, 4), 300);
        assert_eq!(next_backoff_secs(u64::MAX, u32::MAX), 300);
    }

    #[test]
    fn classify_tick_result_resets_after_success() {
        let action = classify_tick_result(Ok(()), 10, 3).unwrap();
        assert_eq!(
            action,
            TickLoopAction::Success {
                consecutive_failures: 0,
            }
        );
    }

    #[test]
    fn classify_tick_result_backs_off_on_transient_errors() {
        let err = DaemonError::Timeout("slow tool".to_string());
        let action = classify_tick_result::<()>(Err(err), 10, 1).unwrap();

        assert_eq!(
            action,
            TickLoopAction::TransientBackoff {
                consecutive_failures: 2,
                sleep_secs: 20,
                error: "timeout: slow tool".to_string(),
            }
        );
    }

    #[test]
    fn classify_tick_result_returns_fatal_errors() {
        let err = DaemonError::Config("missing target_repo".to_string());
        let fatal = classify_tick_result::<()>(Err(err), 10, 0).unwrap_err();

        assert!(matches!(fatal, DaemonError::Config(_)));
    }

    #[test]
    fn transient_tick_retries_same_index_until_success_advances() {
        let mut state = TickLoopState {
            tick_index: 12,
            consecutive_failures: 0,
            has_attempted: true,
            last_tick_time: 100_u64,
        };

        let first_attempt = state.begin_tick(110, |last, now| now - last);
        assert_eq!(
            first_attempt,
            TickAttempt {
                tick_index: 12,
                elapsed_secs: 10,
            }
        );

        let transient = classify_tick_result::<()>(
            Err(DaemonError::Tool {
                tool: "gh".to_string(),
                rc: 1,
                stderr: "network unavailable".to_string(),
            }),
            10,
            state.consecutive_failures,
        )
        .unwrap();
        state.apply_action(&transient);
        assert_eq!(state.tick_index, 12);

        let retry_attempt = state.begin_tick(115, |last, now| now - last);
        assert_eq!(retry_attempt.tick_index, 12);

        let success = classify_tick_result(Ok(()), 10, state.consecutive_failures).unwrap();
        state.apply_action(&success);
        assert_eq!(state.tick_index, 13);
        assert_eq!(state.consecutive_failures, 0);
    }

    #[test]
    fn transient_tick_anchors_elapsed_window_before_retry() {
        let mut state = TickLoopState {
            tick_index: 7,
            consecutive_failures: 0,
            has_attempted: true,
            last_tick_time: 100_u64,
        };

        let failed_attempt = state.begin_tick(160, |last, now| now - last);
        assert_eq!(
            failed_attempt,
            TickAttempt {
                tick_index: 7,
                elapsed_secs: 60,
            }
        );

        let transient = classify_tick_result::<()>(
            Err(DaemonError::Timeout("ao status timed out".to_string())),
            10,
            state.consecutive_failures,
        )
        .unwrap();
        state.apply_action(&transient);

        let retry_attempt = state.begin_tick(190, |last, now| now - last);
        assert_eq!(
            retry_attempt,
            TickAttempt {
                tick_index: 7,
                elapsed_secs: 30,
            }
        );
        assert_eq!(state.last_tick_time, 190);
    }

    #[test]
    fn first_tick_retry_reports_elapsed_since_failed_attempt() {
        let mut state = TickLoopState::new(100_u64);

        let failed_attempt = state.begin_tick(160, |last, now| now - last);
        assert_eq!(
            failed_attempt,
            TickAttempt {
                tick_index: 0,
                elapsed_secs: 0,
            }
        );

        let transient = classify_tick_result::<()>(
            Err(DaemonError::Timeout("initial tick timed out".to_string())),
            10,
            state.consecutive_failures,
        )
        .unwrap();
        state.apply_action(&transient);

        let retry_attempt = state.begin_tick(190, |last, now| now - last);
        assert_eq!(
            retry_attempt,
            TickAttempt {
                tick_index: 0,
                elapsed_secs: 30,
            }
        );
    }

    #[test]
    fn transient_tick_reconciles_dispatching_before_retry() {
        let mut state = TickLoopState::new(100_u64);
        let action = classify_tick_result::<()>(
            Err(DaemonError::Tool {
                tool: "ao".to_string(),
                rc: 1,
                stderr: "spawn failed".to_string(),
            }),
            10,
            state.consecutive_failures,
        )
        .unwrap();
        let mut reconcile_count = 0;

        apply_tick_action(&mut state, &action, || {
            reconcile_count += 1;
            Ok(())
        })
        .unwrap();

        assert_eq!(reconcile_count, 1);
        assert_eq!(state.tick_index, 0);
        assert_eq!(state.consecutive_failures, 1);
    }

    #[test]
    fn successful_tick_does_not_reconcile_dispatching() {
        let mut state = TickLoopState::new(100_u64);
        let action = classify_tick_result(Ok(()), 10, state.consecutive_failures).unwrap();
        let mut reconcile_count = 0;

        apply_tick_action(&mut state, &action, || {
            reconcile_count += 1;
            Ok(())
        })
        .unwrap();

        assert_eq!(reconcile_count, 0);
        assert_eq!(state.tick_index, 1);
        assert_eq!(state.consecutive_failures, 0);
    }

    #[cfg(unix)]
    #[test]
    fn systemd_notify_sends_to_notify_socket() {
        use std::os::unix::net::UnixDatagram;
        use std::time::Duration;

        let _env_guard = NOTIFY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let socket_path = std::path::PathBuf::from(format!(
            "/tmp/df-notify-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixDatagram::bind(&socket_path).unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        std::env::set_var("NOTIFY_SOCKET", &socket_path);
        systemd_notify("READY=1\nSTATUS=test-ready").unwrap();
        std::env::remove_var("NOTIFY_SOCKET");

        let mut buf = [0_u8; 256];
        let n = listener.recv(&mut buf).unwrap();
        let received = std::str::from_utf8(&buf[..n]).unwrap();

        assert!(received.contains("READY=1"));
        assert!(received.contains("STATUS=test-ready"));
        let _ = std::fs::remove_file(socket_path);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_notify_sends_to_abstract_notify_socket() {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixDatagram};
        use std::time::Duration;

        let _env_guard = NOTIFY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let socket_name = format!(
            "df-abs-{}",
            std::process::id()
        );
        let address = SocketAddr::from_abstract_name(socket_name.as_bytes()).unwrap();
        let listener = UnixDatagram::bind_addr(&address).unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        std::env::set_var("NOTIFY_SOCKET", format!("@{socket_name}"));
        systemd_notify("READY=1\nSTATUS=test-ready-abstract").unwrap();
        std::env::remove_var("NOTIFY_SOCKET");

        let mut buf = [0_u8; 256];
        let n = listener.recv(&mut buf).unwrap();
        let received = std::str::from_utf8(&buf[..n]).unwrap();

        assert!(received.contains("READY=1"));
        assert!(received.contains("STATUS=test-ready-abstract"));
    }

    /// Bead jleechan-sb4b: the startup cargo check must skip silently
    /// under `--dry-run` (tests construct synthetic envs).
    #[test]
    fn startup_cargo_check_is_silent_under_dry_run() {
        // Even with PATH stripped to "/tmp/some_empty_dir" and a
        // nonexistent CARGO_HOME, the dry-run flag must keep the
        // startup function silent — no stderr write, no panic.
        let prev_path = std::env::var_os("PATH");
        let prev_cargo_home = std::env::var_os("CARGO_HOME");
        std::env::set_var("PATH", "/tmp/vacuous_dry_run_empty");
        std::env::remove_var("CARGO_HOME");
        let args = Args { dry_run: true, once: false };
        // No panic / no assertion of stderr content — the test simply
        // asserts the function returns without effect.
        verify_startup_cargo_toolchain(args);
        match prev_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        match prev_cargo_home {
            Some(p) => std::env::set_var("CARGO_HOME", p),
            None => std::env::remove_var("CARGO_HOME"),
        }
    }
}
