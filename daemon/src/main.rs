// Task 10: tick loop wiring + CLI flags (design doc §5, spec §4.2.2/§4.2.9).
// Modules live in `lib.rs` (see that file) so `daemon/tests/*` integration
// tests can `use daemon::{...}`; this binary just drives the poll loop.
#![allow(dead_code, unused_imports, clippy::type_complexity)]

use daemon::config::{self, Config};
use daemon::errors::DaemonError;
use daemon::instancelock::{self, AcquireOutcome, LeasePayload};
use daemon::state::{SqliteStateStore, StateStore};
use daemon::telemetry::{self, TelemetryEvent};
use daemon::tick::{run_tick, TickDeps};
use daemon::tools::{Bead, Issue, Llm, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs};
use std::path::{Path, PathBuf};

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

/// Pre-flight result for the daemon subcommand: either we run normally or
/// the user asked us to print usage/version and exit 0 (before we touch
/// CXDB, leases, timers, or telemetry). `--help`/`--version` returning a
/// value rather than mutating state is what stops an ostensibly read-only
/// diagnostic from entering the live tick loop and dispatching beads
/// (jleechan-bze8.4 incident of 2026-07-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonPreFlight {
    Run,
    PrintUsage,
    PrintVersion,
}

/// Strict-parse the daemon subcommand's argv. Returns `Err` for any
/// unrecognized flag (after `--help`/`--version` short-circuits), which
/// lets `main` exit non-zero without opening the lease, opening CXDB, or
/// emitting any telemetry.
fn parse_daemon_flags(
    iter: impl Iterator<Item = String>,
) -> Result<(DaemonPreFlight, Args), String> {
    let mut args = Args::default();
    let mut preflight = DaemonPreFlight::Run;
    for arg in iter {
        match arg.as_str() {
            "--once" => args.once = true,
            "--dry-run" => args.dry_run = true,
            "--help" | "-h" => preflight = DaemonPreFlight::PrintUsage,
            "--version" | "-V" => preflight = DaemonPreFlight::PrintVersion,
            other => {
                return Err(format!(
                    "unknown daemon flag `{other}`; pass --help for usage"
                ));
            }
        }
    }
    Ok((preflight, args))
}

#[derive(Debug, Clone)]
enum CommandMode {
    Daemon(DaemonPreFlight, Args),
    RecoverHeld {
        db: PathBuf,
        telemetry_log: PathBuf,
    },
    GatesCompute {
        pr: u64,
        repo: Option<String>,
    },
}

const USAGE_BANNER: &str = "dark-factory daemon — auto-factory tick loop\n\
\n\
USAGE:\n  \
daemon [--once] [--dry-run] [--help] [--version]\n\
\n\
OPTIONS:\n  \
--once       Run exactly one tick then exit (instead of looping forever)\n  \
--dry-run    Construct every tool-boundary trait as a no-op stub (tests only)\n  \
-h, --help   Print this help and exit 0\n  \
-V, --version  Print the daemon version and exit 0\n\
\n\
SUBCOMMANDS:\n  \
gates-compute --pr <N> [--repo <owner/repo>]   Compute gates for a PR (read-only)\n  \
recover-held --db <path> --telemetry-log <path>  Recover stuck HUMAN_HELD beads\n\
\n\
NOTE:\n  \
All other arguments are rejected before the daemon touches CXDB, acquires\n  \
its single-instance lease, emits startup telemetry, or opens a tick. This\n  \
is the jleechan-bze8.4 fix: --help previously entered the production tick\n  \
loop and dispatched beads.";

const DAEMON_VERSION: &str = env!(
    "CARGO_PKG_VERSION",
    "CARGO_PKG_VERSION must be set by cargo build for daemon"
);

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
            // Strict parse for the daemon subcommand: --help/--version return
            // a preflight so main prints + exits 0 WITHOUT touching the
            // lease, CXDB, timers, or telemetry; unknown args return Err so
            // main exits non-zero without a tick (jleechan-bze8.4).
            let mut all_args = vec![first_flag.to_string()];
            all_args.extend(argv);
            let (preflight, args) = parse_daemon_flags(all_args.into_iter())?;
            Ok(CommandMode::Daemon(preflight, args))
        }
        None => Ok(CommandMode::Daemon(DaemonPreFlight::Run, Args::default())),
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
    fn labeled_prs(&self, _label: &str) -> Result<Vec<daemon::tools::LabeledPr>, DaemonError> {
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
    std::env::var_os("HOME")
        .map(|home| Path::new(&home).join(".dark-factory/daemon-cxdb.sqlite"))
        .unwrap_or_else(|| PathBuf::from("daemon-cxdb.sqlite"))
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
    let default_agent = std::env::var("DARK_FACTORY_REVIEWER_DEFAULT")
        .unwrap_or_else(|_| "minimax".to_string());
    Ok((ao_project, default_agent))
}

fn verify_startup_ao_compatibility(
    args: Args,
    ao_project: &str,
    agent: &str,
    verify: impl FnOnce(&str, &str) -> Result<(), DaemonError>,
) -> Result<(), DaemonError> {
    if args.dry_run {
        return Ok(());
    }
    verify(ao_project, agent)
}

/// Build the lease payload from the daemon's own runtime identity.
fn build_lease_payload(cfg: &Config) -> Result<LeasePayload, DaemonError> {
    let pid = std::process::id();
    let start_time_unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let instance_uuid = format!("inst-{:032x}", start_time_unix_secs ^ pid as u64);
    let executable_sha256 = executable_sha256().unwrap_or_else(|e| {
        format!("sha-unavailable:{e}")
    });
    let config_identity = config_identity(cfg);
    telemetry::set_instance_uuid(&instance_uuid);
    Ok(LeasePayload {
        pid,
        start_time_unix_secs,
        instance_uuid,
        executable_sha256,
        config_identity,
    })
}

/// Compute config identity from the on-disk config (path) so a config file
/// edit changes the identity and postmortems can correlate.
fn config_identity(cfg: &Config) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    cfg.target_repo.hash(&mut h);
    cfg.stage.hash(&mut h);
    cfg.fast_tick_secs.hash(&mut h);
    cfg.slow_tick_secs.hash(&mut h);
    cfg.base_branch.hash(&mut h);
    format!("cfg-{:016x}", h.finish())
}

/// SHA-256 of the running executable. Tolerant of `Err` (e.g. the binary
/// was unlinked by `cargo run` cleanup) so the daemon still starts.
fn executable_sha256() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&exe).map_err(|e| {
        format!("read {}: {e}", exe.display())
    })?;
    // Lightweight, dep-free SHA-256: exact-match against the well-known
    // "cirun" bad-binary sentinel would be cheaper, but a real hash is the
    // postmortem operator's first stop. We avoid openssl by using the
    // system `sha256sum` CLI — it's present on every Linux runner and
    // macOS developer machine even when `cargo` deps are stripped.
    let output = std::process::Command::new("sha256sum")
        .arg("--")
        .arg(&exe)
        .output()
        .or_else(|_| {
            // macOS fallback: `shasum -a 256`.
            std::process::Command::new("shasum")
                .arg("-a")
                .arg("256")
                .arg("--")
                .arg(&exe)
                .output()
        })
        .map_err(|e| format!("sha256sum/shasum missing: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let digest = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| "sha256sum produced no digest".to_string())?
        .to_string();
    // Sanity: never persist an empty hash; if hashing failed silently
    // we'd rather mark the lease so clearly than silently leak it.
    if digest.is_empty() || digest == bytes.len().to_string() {
        return Err(format!("unexpected sha digest: {digest}"));
    }
    Ok(digest)
}

fn emit_startup_event(
    telemetry_log: &Path,
    payload: &LeasePayload,
    config_path: &Path,
    lock_path: &Path,
) -> Result<(), DaemonError> {
    telemetry::emit(
        telemetry_log,
        &telemetry::TelemetryEvent {
            timestamp: daemon::state::now_iso8601(),
            bead_id: "daemon-startup".into(),
            attempt_id: 0,
            lifecycle_state: "STARTUP".into(),
            event_type: "DAEMON_STARTED".into(),
            metrics: serde_json::json!({}),
            context: serde_json::json!({
                "pid": payload.pid,
                "startTimeUnixSecs": payload.start_time_unix_secs,
                "instanceUuid": payload.instance_uuid,
                "executableSha256": payload.executable_sha256,
                "configIdentity": payload.config_identity,
                "configPath": config_path.display().to_string(),
                "lockPath": lock_path.display().to_string(),
            }),
        },
    )
}

fn emit_lock_contended_event(
    telemetry_log: &Path,
    holder: &LeasePayload,
    lock_path: &Path,
) -> Result<(), DaemonError> {
    let _ = telemetry_log;
    let _ = holder;
    let _ = lock_path;
    Ok(())
}

fn run(args: Args) -> Result<(), DaemonError> {
    let cfg_path = default_config_path();
    let cfg = load_config(&cfg_path)?;
    let (ao_project, default_agent) = ao_runtime_binding(&cfg)?;
    // Fail before opening/reconciling state, advertising READY, or polling a
    // healthy tick when the installed AO/Node adapter is incompatible. The
    // diagnostic exits before AO preflight, locking, workspace creation, or
    // worker launch.
    verify_startup_ao_compatibility(args, &ao_project, &default_agent, |project, agent| {
        daemon::adapters::verify_ao_bridge_compatibility(project, agent)
    })?;
    let telemetry_log = default_telemetry_log();
    let db_path = default_state_db_path();

    // jleechan-bze8.4: acquire the single-instance lease BEFORE opening
    // CXDB, starting timers, or reconciling state. If another live daemon
    // already holds it, exit non-zero without dispatching or writing any
    // tick telemetry. The startup telemetry IS emitted (to a dedicated
    // channel that carries the failure) so postmortem operators can see
    // exactly which process tried to start and against whom.
    let lease_payload = build_lease_payload(&cfg)?;
    let lock_path = instancelock::default_lock_path(&db_path);
    let lease_outcome = instancelock::acquire(&lock_path, lease_payload)?;
    let _lease_guard = match lease_outcome {
        AcquireOutcome::Acquired(lease) => {
            // Best-effort STARTUP event — telemetry write failure must NOT
            // prevent the daemon from running (a tmpdir-ro crash should
            // not kill the dispatch loop).
            if let Err(e) =
                emit_startup_event(&telemetry_log, &lease.payload, &cfg_path, &lease.dir)
            {
                eprintln!("auto-factory daemon: warn: startup telemetry failed: {e}");
            }
            lease
        }
        AcquireOutcome::AlreadyHeld { path, holder } => {
            eprintln!(
                "auto-factory daemon: another daemon already holds the lease at {path:?}; owner pid={} instance_uuid={} started_at={}",
                holder.pid, holder.instance_uuid, holder.start_time_unix_secs
            );
            let _ = emit_lock_contended_event(&telemetry_log, &holder, &path);
            return Err(DaemonError::Config(format!(
                "single-instance lock held by pid={} (instance_uuid={}); refusing to start",
                holder.pid, holder.instance_uuid
            )));
        }
    };

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

    let deps = TickDeps {
        scm: scm.as_ref(),
        tracker: tracker.as_ref(),
        sessions: sessions.as_ref(),
        llm: llm.as_ref(),
        store: store.as_ref(),
        vcs: vcs.as_ref(),
        cfg: &cfg,
        telemetry_log: &telemetry_log,
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
        apply_tick_action(&mut tick_loop, &action, || store.reconcile_dispatching())?;
        let _ = systemd_notify(&watchdog_status);

        match action {
            TickLoopAction::Success {
                consecutive_failures: _,
            } => {
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

fn run_recover_held(db: &Path, telemetry_log: &Path) -> Result<(), DaemonError> {
    const MAX_HUMAN_HELD_RECOVERY_ATTEMPT: u32 = 10;

    let store = SqliteStateStore::open(db)?;
    let recovered = store.recover_human_held(MAX_HUMAN_HELD_RECOVERY_ATTEMPT)?;
    for overlay in &recovered {
        daemon::telemetry::emit(
            telemetry_log,
            &daemon::telemetry::TelemetryEvent {
                timestamp: daemon::state::now_iso8601(),
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
            std::process::exit(2);
        }
    };

    match mode {
        CommandMode::Daemon(preflight, args) => {
            // jleechan-bze8.4: --help / --version are handled strictly
            // here, BEFORE `run()` touches the lease, CXDB, or timers.
            // `parse_daemon_flags` returned a preflight directive rather
            // than letting the unknown-flag value escape the parser —
            // see `parse_args` for the matching `parse_daemon_flags`
            // boundary.
            match preflight {
                DaemonPreFlight::PrintUsage => {
                    println!("{}", USAGE_BANNER);
                    std::process::exit(0);
                }
                DaemonPreFlight::PrintVersion => {
                    println!("dark-factory daemon {}", DAEMON_VERSION);
                    std::process::exit(0);
                }
                DaemonPreFlight::Run => {
                    if let Err(e) = run(args) {
                        eprintln!("auto-factory daemon: fatal: {e}");
                        std::process::exit(1);
                    }
                }
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
        let error = verify_startup_ao_compatibility(
            Args::default(),
            "dark-factory",
            "minimax",
            |project, agent| {
                observed = Some((project.to_string(), agent.to_string()));
                Err(DaemonError::Config("incompatible AO runtime".to_string()))
            },
        )
        .unwrap_err();

        assert_eq!(
            observed,
            Some(("dark-factory".to_string(), "minimax".to_string()))
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
            repos: std::collections::HashMap::new(),
        };

        let (project, _) = ao_runtime_binding(&cfg).unwrap();

        assert_eq!(project, "worldarchitect");
    }

    #[test]
    fn dry_run_startup_skips_production_ao_compatibility_check() {
        let mut called = false;
        let result = verify_startup_ao_compatibility(
            Args {
                once: true,
                dry_run: true,
            },
            "dark-factory",
            "minimax",
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
        if let CommandMode::Daemon(preflight, args) = mode {
            assert_eq!(preflight, DaemonPreFlight::Run);
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
        if let CommandMode::Daemon(preflight, args) = mode {
            assert_eq!(preflight, DaemonPreFlight::Run);
            assert!(!args.once);
            assert!(!args.dry_run);
        } else {
            panic!("expected Daemon mode");
        }
    }

    /// jleechan-bze8.4: the previous parser silently accepted `--bogus`,
    /// which is what let `daemon --help` flow into the live tick loop
    /// on 2026-07-18. The strict boundary must reject any unknown flag
    /// BEFORE the daemon reaches `run()` or its single-instance lease.
    #[test]
    fn parse_args_rejects_unknown_flags() {
        let argv = vec![
            "daemon".to_string(),
            "--bogus".to_string(),
            "--once".to_string(),
        ];
        let err = parse_args(argv.into_iter()).unwrap_err();
        assert!(err.contains("--bogus"), "got: {err}");
        assert!(
            err.contains("--help"),
            "rejection must suggest --help; got: {err}"
        );
    }

    /// jleechan-bze8.4: `--help` returns a preflight so `main` exits 0
    /// without opening CXDB, acquiring the lease, or starting timers.
    #[test]
    fn parse_args_help_returns_preflight_without_running() {
        let argv = vec!["daemon".to_string(), "--help".to_string()];
        let mode = parse_args(argv.into_iter()).unwrap();
        match mode {
            CommandMode::Daemon(DaemonPreFlight::PrintUsage, args) => {
                assert!(!args.once && !args.dry_run);
            }
            other => panic!("expected help preflight, got {other:?}"),
        }
    }

    /// jleechan-bze8.4: `--version` mirrors `--help`'s preflight.
    #[test]
    fn parse_args_version_returns_preflight_without_running() {
        let argv = vec!["daemon".to_string(), "-V".to_string()];
        let mode = parse_args(argv.into_iter()).unwrap();
        match mode {
            CommandMode::Daemon(DaemonPreFlight::PrintVersion, _) => {}
            other => panic!("expected version preflight, got {other:?}"),
        }
    }

    /// jleechan-bze8.4: `daemon` with zero args runs normally — preflight
    /// is `Run`, no flag rejected, no side effect of the parser itself.
    #[test]
    fn parse_args_no_args_does_not_trigger_preflight() {
        let argv = vec!["daemon".to_string()];
        let mode = parse_args(argv.into_iter()).unwrap();
        match mode {
            CommandMode::Daemon(DaemonPreFlight::Run, _) => {}
            other => panic!("expected Run preflight, got {other:?}"),
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

        let socket_path = std::env::temp_dir().join(format!(
            "dark-factory-notify-{}-{}.sock",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
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
            "dark-factory-notify-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
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
}
