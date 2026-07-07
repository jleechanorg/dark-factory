// Task 10: tick loop wiring + CLI flags (design doc §5, spec §4.2.2/§4.2.9).
// Modules live in `lib.rs` (see that file) so `daemon/tests/*` integration
// tests can `use daemon::{...}`; this binary just drives the poll loop.
#[allow(dead_code, unused_imports)]
use daemon::config::{self, Config};
use daemon::errors::DaemonError;
use daemon::state::{SqliteStateStore, StateStore};
use daemon::tick::{run_tick, TickDeps};
use daemon::tools::{Bead, Issue, Llm, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs};
use std::path::{Path, PathBuf};

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
    GatesCompute {
        pr: u64,
        repo: Option<String>,
    },
}

fn parse_args(mut argv: impl Iterator<Item = String>) -> Result<CommandMode, String> {
    let _bin_name = argv.next();
    let next_arg = argv.next();
    match next_arg.as_deref() {
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

type DaemonAdapters = (
    Box<dyn Scm>,
    Box<dyn Tracker>,
    Box<dyn Sessions>,
    Box<dyn Llm>,
    Box<dyn Vcs>,
);

fn run(args: Args) -> Result<(), DaemonError> {
    let cfg_path = default_config_path();
    let cfg = load_config(&cfg_path)?;
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
            Box::new(CliSessions::new(&cfg.target_repo, "claude-code")),
            Box::new(ChainLlm),
            Box::new(CliVcs),
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
        run_tick(&deps, 0, 0)?;
        return Ok(());
    }

    let mut tick_index: u64 = 0;
    let mut last_tick_time = std::time::Instant::now();
    loop {
        let now = std::time::Instant::now();
        let elapsed_secs = if tick_index == 0 {
            0
        } else {
            now.duration_since(last_tick_time).as_secs()
        };
        last_tick_time = now;

        run_tick(&deps, tick_index, elapsed_secs)?;
        tick_index += 1;
        std::thread::sleep(std::time::Duration::from_secs(cfg.fast_tick_secs));
    }
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_compiles() {
        let ok = true;
        assert!(ok);
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
}
