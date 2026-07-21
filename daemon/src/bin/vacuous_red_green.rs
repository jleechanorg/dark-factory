// CLI front-end for `daemon::vacuous_red_green` — runtime red-green
// vacuous-test detector. Issue #387 / bead jleechan-ijod. Companion to
// the static detector (`daemon::vacuous`). Issue #408 r3: takes an
// explicit `--manifest-path` (P1-3) so cargo can locate the daemon
// Cargo.toml on dark-factory layout.
//
// Usage:
//   vacuous_red_green --base <ref> [--manifest-path <Cargo.toml>]
//                      [--files P ...] [--json OUT]
//
// Exits:
//   0 — at least one new/changed test FAILS on the reverted tree AND all
//       three checks (green_on_head, failed_on_revert, baseline_passed)
//       pass (genuine red-green; gate passes)
//   1 — every new/changed test PASSES on the reverted tree, OR one of the
//       three checks fails (vacuous; gate fails)
//   2 — internal error (diff capture, git apply, etc.)

use daemon::vacuous_red_green::{check_red_green, to_gate_status, FileClass};
use daemon::verifier::VacuousRedGreenStatus;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let mut base: Option<String> = None;
    let mut manifest: Option<PathBuf> = None;
    let mut files: Vec<String> = Vec::new();
    let mut json_out: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--base" => {
                base = args.next();
            }
            "--manifest-path" => {
                manifest = args.next().map(PathBuf::from);
            }
            "--files" => {
                while let Some(next) = args.peek() {
                    if next.starts_with("--") {
                        break;
                    }
                    files.push(args.next().unwrap());
                }
            }
            "--json" => {
                json_out = args.next().map(PathBuf::from);
            }
            "-h" | "--help" => {
                eprintln!(
                    "vacuous_red_green --base <ref> [--manifest-path <Cargo.toml>] \
                     [--files P ...] [--json OUT]"
                );
                std::process::exit(2);
            }
            other => {
                eprintln!("vacuous_red_green: unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let base = match base {
        Some(b) => b,
        None => {
            eprintln!("vacuous_red_green: --base <ref> is required");
            std::process::exit(2);
        }
    };

    // P1-3: default to `<cwd>/daemon/Cargo.toml` if `--manifest-path` is
    // absent. On dark-factory layout that is the only Cargo.toml in the
    // repo. The check on existence is delegated to `check_red_green` so
    // the test surface (which supplies an explicit path) still works.
    let cwd = std::env::current_dir().expect("cwd");
    let manifest_path = manifest.unwrap_or_else(|| cwd.join("daemon").join("Cargo.toml"));

    let changed = if files.is_empty() {
        derive_changed(&cwd, &base)
    } else {
        files
            .iter()
            .map(|p| (cwd.join(p), classify_path(p)))
            .collect::<Vec<_>>()
    };

    let report = match check_red_green(&cwd, &manifest_path, &base, &changed) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("vacuous_red_green: error: {e}");
            std::process::exit(2);
        }
    };

    if let Some(path) = json_out {
        let body = serde_json::to_string_pretty(&report).unwrap_or_else(|e| {
            eprintln!("vacuous_red_green: json serialize failed: {e}");
            std::process::exit(2);
        });
        if let Err(e) = std::fs::write(&path, body) {
            eprintln!("vacuous_red_green: write {}: {e}", path.display());
            std::process::exit(2);
        }
    }

    // r5 (CodeRabbit review of PR #420): reuse `to_gate_status` as the
    // single source of truth for the CLI's exit status so the CLI and
    // the daemon gate path cannot drift. r3 had an inline
    // `any_check_failed` expression that contradicted its own comment
    // ("flagged but NOT a fatal error") and used a bool-cast
    // (`len() > is_empty() as usize`) comparison that almost never
    // fired. Pending and Failed both exit 1; only Verified exits 0.
    let status = to_gate_status(&report);
    match status {
        VacuousRedGreenStatus::Verified => {
            eprintln!(
                "vacuous_red_green: GENUINE ({} tests targeted, {} failed on revert: {:?}, \
                 green_on_head=true, baseline_passed=true)",
                report.targeted_tests.len(),
                report.failing_tests.len(),
                report.failing_tests
            );
            std::process::exit(0);
        }
        VacuousRedGreenStatus::Failed(reason) => {
            eprintln!(
                "vacuous_red_green: FAILED ({reason}; vacuous={}, green_on_head={}, \
                 baseline_passed={}, targeted={}, failed_on_revert={}, ignored_no_reason={:?})",
                report.vacuous,
                report.green_on_head,
                report.baseline_passed,
                report.targeted_tests.len(),
                report.failing_tests.len(),
                report.ignored_without_skip_reason
            );
            std::process::exit(1);
        }
        VacuousRedGreenStatus::Pending(reason) => {
            eprintln!(
                "vacuous_red_green: PENDING ({reason}; vacuous={}, green_on_head={}, \
                 baseline_passed={}, targeted={}, failed_on_revert={}, ignored_no_reason={:?})",
                report.vacuous,
                report.green_on_head,
                report.baseline_passed,
                report.targeted_tests.len(),
                report.failing_tests.len(),
                report.ignored_without_skip_reason
            );
            std::process::exit(1);
        }
        VacuousRedGreenStatus::NotRun => {
            // Detector did not run (operator disabled, missing manifest,
            // etc.) — surface as 2 (internal/inconclusive) so the calling
            // shell script can distinguish "no answer" from "definitely
            // vacuous".
            eprintln!("vacuous_red_green: NOT_RUN (detector was not executed)");
            std::process::exit(2);
        }
    }
}

fn derive_changed(cwd: &std::path::Path, base: &str) -> Vec<(PathBuf, FileClass)> {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["diff", "--name-only", &format!("{base}...HEAD")])
        .output();
    let stdout = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(),
    };
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| (cwd.join(l), classify_path(l)))
        .collect()
}

/// Heuristic file classifier. A path is a Test file if any of the
/// following is true (checked against the repo-relative path):
///
/// * contains `/tests/`
/// * starts with `tests/` (cargo convention for the integration test dir)
/// * ends with `/tests` (a directory entry, in case `--files` is a dir)
/// * ends with `_test.rs` (cargo convention for `#[cfg(test)]` modules)
///
/// Anything else is classified as Production.
fn classify_path(path: &str) -> FileClass {
    let normalized = path.trim_start_matches('/').trim_start_matches("./");
    if path.contains("/tests/")
        || path.starts_with("tests/")
        || path.ends_with("/tests")
        || normalized.starts_with("tests/")
        || path.ends_with("_test.rs")
    {
        FileClass::Test
    } else {
        FileClass::Production
    }
}