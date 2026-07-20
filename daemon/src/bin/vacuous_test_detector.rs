// CLI front-end for `daemon::vacuous`. PR #387 / bead jleechan-ijod.
// See `daemon/scripts/vacuous-test-detector.sh` for the shell wrapper that
// invokes this binary.
//
// Arguments:
//   --paths <P>     (repeatable) A path (file or directory) to scan; any
//                   `*.rs` file encountered is scanned. Required.
//   --json <FILE>   Write the merged `ScanReport` to <FILE> as JSON. When
//                   omitted, the report goes to stdout.
//
// Exit codes:
//   0 — no findings (clean, gate pass)
//   1 — at least one finding (gate fail; auto-merge-guard refuses to merge)
//   2 — internal error (treated as infra failure by the gate)

use daemon::vacuous::{scan_test_directory, scan_test_file, VacuousFinding};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut json_out: Option<PathBuf> = None;
    // Two-pass parse: first identify "--paths P P P --json J / --help" sequences.
    // We treat any argument that does NOT start with "--" and whose immediate
    // predecessor was "--paths" (or another non-flag arg under --paths) as a
    // path to scan.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].as_str();
        match arg {
            "--paths" => {
                i += 1;
                while i < raw.len() && !raw[i].starts_with("--") {
                    paths.push(PathBuf::from(&raw[i]));
                    i += 1;
                }
            }
            "--json" => {
                if i + 1 < raw.len() {
                    json_out = Some(PathBuf::from(&raw[i + 1]));
                    i += 2;
                } else {
                    eprintln!("vacuous_test_detector: --json requires a value");
                    return ExitCode::from(2);
                }
            }
            "--help" | "-h" => {
                eprintln!("vacuous_test_detector --paths <P> [P ...] [--json <file>]");
                return ExitCode::from(0);
            }
            other => {
                eprintln!("vacuous_test_detector: unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
    }

    if paths.is_empty() {
        eprintln!("vacuous_test_detector: no --paths supplied");
        return ExitCode::from(2);
    }

    let mut all_findings: Vec<VacuousFinding> = Vec::new();
    let mut files_scanned = 0;
    let mut errors: Vec<String> = Vec::new();
    for p in &paths {
        if p.is_file() {
            match scan_test_file(p) {
                Ok(r) => {
                    all_findings.extend(r.findings);
                    files_scanned += r.files_scanned;
                }
                Err(e) => errors.push(format!("{}: {e:?}", p.display())),
            }
        } else if p.is_dir() {
            match scan_test_directory(p) {
                Ok(r) => {
                    all_findings.extend(r.findings);
                    files_scanned += r.files_scanned;
                }
                Err(e) => errors.push(format!("{}: {e:?}", p.display())),
            }
        } else {
            // missing or unreadable path: skip quietly so the wrapper can
            // pass a list of paths from `git diff` (some of which may have
            // been renamed/deleted).
        }
    }

    if let Some(out_path) = &json_out {
        // Hand-roll the JSON so we don't need a Serialize derive on the
        // library types (the library exposes typed findings for in-process
        // callers; the CLI serializes them by hand).
        let mut buf = String::new();
        buf.push_str("{\n");
        buf.push_str(&format!("  \"files_scanned\": {files_scanned},\n"));
        buf.push_str("  \"findings\": [\n");
        for (i, f) in all_findings.iter().enumerate() {
            if i > 0 {
                buf.push_str(",\n");
            }
            buf.push_str(&format!(
                "    {{\"file\": \"{}\", \"line\": {}, \"kind\": \"{:?}\", \"snippet\": \"{}\"}}",
                f.file.display(),
                f.line,
                f.kind,
                f.snippet.replace('"', "\\\"")
            ));
        }
        buf.push_str("\n  ],\n");
        buf.push_str("  \"errors\": [");
        for (i, e) in errors.iter().enumerate() {
            if i == 0 {
                buf.push('\n');
            } else {
                buf.push_str(",\n");
            }
            buf.push_str(&format!("    \"{}\"", e.replace('"', "\\\"")));
        }
        if !errors.is_empty() {
            buf.push('\n');
        }
        buf.push_str("  ]\n}\n");
        if let Err(e) = std::fs::write(out_path, buf) {
            eprintln!("vacuous_test_detector: write {}: {e}", out_path.display());
            return ExitCode::from(2);
        }
    } else {
        for f in &all_findings {
            println!(
                "{}:{}  {:?}  {}",
                f.file.display(),
                f.line,
                f.kind,
                f.snippet
            );
        }
        for e in &errors {
            eprintln!("vacuous_test_detector: error: {e}");
        }
    }

    if !errors.is_empty() && all_findings.is_empty() {
        // Infra failure only — no findings to report, but errors indicate a
        // misconfiguration or filesystem issue. Surface as exit code 2.
        return ExitCode::from(2);
    }
    if all_findings.is_empty() {
        ExitCode::from(0)
    } else {
        ExitCode::from(1)
    }
}

#[allow(dead_code)]
fn _force_link(_: &Path) {}
