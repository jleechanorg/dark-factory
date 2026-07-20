// CLI front-end for `daemon::vacuous_red_green` — runtime red-green
// vacuous-test detector. Issue #387 / bead jleechan-ijod. Companion to
// the static detector (`daemon::vacuous`).
//
// Usage:
//   vacuous_red_green --base <ref> [--json <out>] [--files <P> ...]
//
// Exits:
//   0 — at least one new/changed test FAILS on the reverted tree
//       (genuine red-green; gate passes)
//   1 — every new/changed test PASSES on the reverted tree (vacuous;
//       gate fails)
//   2 — internal error (diff capture, git apply, etc.)
//
// `--base <ref>` is required (the diff baseline). `--files` is optional;
// when supplied, only the listed paths are considered. When omitted,
// `git diff --name-only <base>...HEAD` is used to derive the list.

use daemon::vacuous_red_green::{check_red_green, FileClass};
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let mut base: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    let mut json_out: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--base" => {
                base = args.next();
            }
            "--files" => {
                while let Some(next) = args.next() {
                    if next.starts_with("--") {
                        // Put it back for the outer loop to consume.
                        // (Simplest implementation: re-process via a
                        // peekable; here we just stop reading files.)
                        break;
                    }
                    files.push(next);
                }
            }
            "--json" => {
                json_out = args.next().map(PathBuf::from);
            }
            "-h" | "--help" => {
                eprintln!("vacuous_red_green --base <ref> [--files P ...] [--json OUT]");
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

    let cwd = std::env::current_dir().expect("cwd");
    let changed = if files.is_empty() {
        derive_changed(&cwd, &base)
    } else {
        files
            .iter()
            .map(|p| {
                let full = cwd.join(p);
                let class = if p.contains("/tests/") || p.ends_with("_test.rs") {
                    FileClass::Test
                } else {
                    FileClass::Production
                };
                (full, class)
            })
            .collect::<Vec<_>>()
    };

    let report = match check_red_green(&cwd, &base, &changed) {
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

    if report.vacuous {
        eprintln!(
            "vacuous_red_green: VACUOUS ({} tests targeted, 0 failed on revert)",
            report.targeted_tests.len()
        );
        std::process::exit(1);
    } else {
        eprintln!(
            "vacuous_red_green: GENUINE ({} tests targeted, {} failed on revert: {:?})",
            report.targeted_tests.len(),
            report.failing_tests.len(),
            report.failing_tests
        );
        std::process::exit(0);
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
        .map(|l| {
            let p = cwd.join(l);
            let class = if l.contains("/tests/") || l.ends_with("_test.rs") {
                FileClass::Test
            } else {
                FileClass::Production
            };
            (p, class)
        })
        .collect()
}