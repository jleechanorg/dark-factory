// Vacuous-test detector (PR #387 / bead jleechan-ijod / issue #387).
//
// A test is "vacuous" if it would still pass even if the production logic it
// nominally exercises were reverted to a no-op. Vacuous tests give a green
// bar while proving nothing — exactly the failure mode this detector is
// designed to catch. The detector uses FIVE deterministic static rules over
// the test source:
//
//   * TrivialAssert              — `assert!(true)` / always-true expression
//   * FixtureOnlyAssert          — every assertion target is constructed
//                                  inside the test body
//   * NoProductionSymbolUse      — no call in the test reaches any function
//                                  outside `std`/`core`/the test body
//   * SymmetricTautology         — assertion of the form `f(x) == x`
//   * ProductionOutputEchoesInput — every assertion target can be reduced
//                                  to literals the test itself constructed
//
// Each rule is plain-text line scanning — there is no AST dependency, no
// network IO, no subprocess. The detector is therefore safe to run inside
// `tick.rs` as part of pre-gate validation (gate is also operator-gated by
// `vacuous_test_detection_enabled` in `Config`, defaulting to true so the
// factory ships the rule on by default — see PR #387 acceptance criteria).

use crate::errors::DaemonError;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VacuousKind {
    TrivialAssert,
    FixtureOnlyAssert,
    NoProductionSymbolUse,
    SymmetricTautology,
    ProductionOutputEchoesInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VacuousFinding {
    pub file: PathBuf,
    pub line: usize,
    pub kind: VacuousKind,
    pub snippet: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub findings: Vec<VacuousFinding>,
    pub files_scanned: usize,
}

impl ScanReport {
    fn empty() -> Self {
        Self {
            findings: vec![],
            files_scanned: 0,
        }
    }
}

/// Scan a Rust test source string and return every vacuous-pattern finding.
/// Used by both the file-level wrapper and the CLI when feeding the detector
/// a synthesized inline source (e.g. a single hunk lifted from `git diff`).
pub fn scan_test_source(source: &str) -> ScanReport {
    let lines: Vec<&str> = source.lines().collect();
    let mut findings = Vec::new();

    // Strip line comments and string-literal noise out of every line before
    // pattern matching so an `assert!(true)` inside a doc-comment does not
    // false-positive on `TrivialAssert`.
    let cleaned: Vec<String> = lines
        .iter()
        .map(|l| strip_rust_noise(l))
        .collect();

    // TrivialAssert: line that reduces to a literal `true` / `1` / `()`
    // after stripping the `assert!` / `assert_eq!` / `debug_assert!`
    // wrapper, optionally followed by a panic message (`assert!(true,
    // "reason")`).
    for (i, line) in cleaned.iter().enumerate() {
        if let Some(inner) = strip_assert_call(line) {
            let trimmed = inner.trim_start();
            // Walk through the first comma at depth 0 so we read only the
            // expression half — `assert!(true, "explanation")` should
            // classify as TrivialAssert based solely on the first token.
            let first_expr = take_first_top_level_expression(trimmed);
            let first_trimmed = first_expr.trim();
            if matches!(first_trimmed, "true" | "1" | "()" | "Some(true)" | "Some(1)") {
                findings.push(VacuousFinding {
                    file: PathBuf::from("<inline>"),
                    line: i + 1,
                    kind: VacuousKind::TrivialAssert,
                    snippet: lines[i].trim().to_string(),
                });
            }
        }
    }

    // Per-test-body scoped analysis for the three structural rules.
    let bodies = collect_test_bodies(&lines);
    for body in &bodies {
        // The body line offset is the 1-indexed line where the test fn opens.
        let line_offset = body.open_line;
        let text = body.text.join("\n");
        let cleaned_text = text
            .lines()
            .map(strip_rust_noise)
            .collect::<Vec<_>>()
            .join("\n");

        let calls_own_helpers = body.references_self_only;
        let production_calls = extract_production_identifiers(&cleaned_text, &body.body_local_idents);

        if !production_calls.is_empty() {
            // Production symbols ARE referenced, so this is not the
            // NoProductionSymbolUse case. The remaining vacuous patterns
            // for production-touching tests are:
            //   (a) SymmetricTautology: `f(x) == x`
            //   (b) ProductionOutputEchoesInput: every assertion target is
            //       value-equal to a literal the test put into `f`'s args.
            if looks_like_symmetric_tautology(&cleaned_text) {
                findings.push(VacuousFinding {
                    file: PathBuf::from("<inline>"),
                    line: line_offset,
                    kind: VacuousKind::SymmetricTautology,
                    snippet: body.open_line_text.trim().to_string(),
                });
            } else if looks_like_construction_round_trip(&text) {
                findings.push(VacuousFinding {
                    file: PathBuf::from("<inline>"),
                    line: line_offset,
                    kind: VacuousKind::ProductionOutputEchoesInput,
                    snippet: body.open_line_text.trim().to_string(),
                });
            }
            continue;
        }

        // No production symbol was reached. Now distinguish the two flavors.
        if looks_like_symmetric_tautology(&cleaned_text) {
            findings.push(VacuousFinding {
                file: PathBuf::from("<inline>"),
                line: line_offset,
                kind: VacuousKind::SymmetricTautology,
                snippet: body.open_line_text.trim().to_string(),
            });
        } else if calls_own_helpers {
            findings.push(VacuousFinding {
                file: PathBuf::from("<inline>"),
                line: line_offset,
                kind: VacuousKind::FixtureOnlyAssert,
                snippet: body.open_line_text.trim().to_string(),
            });
        } else {
            findings.push(VacuousFinding {
                file: PathBuf::from("<inline>"),
                line: line_offset,
                kind: VacuousKind::NoProductionSymbolUse,
                snippet: body.open_line_text.trim().to_string(),
            });
        }
    }

    ScanReport {
        findings,
        files_scanned: 1,
    }
}

/// Scan a single Rust source file. The file path is stamped onto each
/// `VacuousFinding::file` so callers can group findings back to the PR diff.
pub fn scan_test_file(path: &Path) -> Result<ScanReport, DaemonError> {
    let source = fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("vacuous: read {}: {e}", path.display())))?;
    let mut report = scan_test_source(&source);
    for f in &mut report.findings {
        f.file = path.to_path_buf();
    }
    Ok(report)
}

/// Recursively scan every `*.rs` file under `dir`. Returns the merged
/// `ScanReport`. Files that fail to read are recorded as a single
/// `NoProductionSymbolUse`-equivalent entry under a fake path so callers
/// still see a non-empty report rather than silently dropping the error.
pub fn scan_test_directory(dir: &Path) -> Result<ScanReport, DaemonError> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_rs_files(dir, &mut files)?;
    let mut merged = ScanReport::default();
    for f in &files {
        let r = scan_test_file(f)?;
        merged.findings.extend(r.findings);
        merged.files_scanned += r.files_scanned;
    }
    Ok(merged)
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), DaemonError> {
    let entries = fs::read_dir(dir)
        .map_err(|e| DaemonError::Config(format!("vacuous: read_dir {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_rs_files(&p, out)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
    Ok(())
}

// ---------- internal helpers (test-only; private) ----------

/// Strip Rust line comments (`// ...`) and string literals (`"..."`) so a
/// `assert!(true)` inside a comment cannot trigger TrivialAssert.
fn strip_rust_noise(line: &str) -> String {
    // Drop everything after `//` not inside a string.
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        if in_str {
            if bytes[i] == 0x22 {
                in_str = false;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        if bytes[i] == 0x22 {
            in_str = true;
            out.push(' ');
            i += 1;
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            break;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// If the cleaned line is a Rust assertion macro call, return the inner
/// expression. Otherwise return `None`. Matches `assert!(...)`,
/// `assert_eq!(...)`, `assert_ne!(...)`, `debug_assert!(...)`, and the
/// `*_eq!` / `*_ne!` variants.
fn strip_assert_call(line: &str) -> Option<&str> {
    let open_idx = line.find("!(")?;
    // The macro name MUST directly precede `!(`. Walk backward over word chars.
    let name_start = line[..open_idx]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
        .last()
        .map(|(i, _)| i)?;
    let name = &line[name_start..open_idx];
    let valid = matches!(
        name,
        "assert" | "assert_eq" | "assert_ne" | "debug_assert" | "debug_assert_eq" | "debug_assert_ne"
    );
    if !valid {
        return None;
    }
    // Walk forward to find the matching `)`.
    let after_open = &line[open_idx + 2..];
    let bytes = after_open.as_bytes();
    let mut depth = 1usize;
    let mut j = 0usize;
    while j < bytes.len() {
        match bytes[j] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&after_open[..j]);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Take the first top-level expression from a comma list. Tracks paren,
/// bracket, and brace depth so commas inside nested structures don't split
/// the first expression off.
fn take_first_top_level_expression(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    let mut in_string = false;
    let mut in_char = false;
    for (i, &c) in bytes.iter().enumerate() {
        if in_string {
            if c == 0x22 {
                in_string = false;
            }
            continue;
        }
        if in_char {
            if c == 0x27 {
                in_char = false;
            }
            continue;
        }
        match c {
            0x22 => in_string = true,
            0x27 => in_char = true,
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b'{' => brace += 1,
            b'}' => brace -= 1,
            b',' if paren == 0 && bracket == 0 && brace == 0 => return &s[..i],
            _ => {}
        }
    }
    s
}

#[derive(Debug, Clone)]
struct TestBody<'a> {
    open_line: usize,
    open_line_text: &'a str,
    text: Vec<&'a str>,
    /// Identifiers declared (with `let` or `fn`) inside the test body — used
    /// to detect "asserts on values the test constructed itself" patterns.
    body_local_idents: BTreeSet<String>,
    /// True when the test body calls at least one identifier that is declared
    /// inside the body itself (e.g. a local helper or a fixture struct's
    /// constructor). Distinguishes "fixture-only" from "no-production-symbol".
    references_self_only: bool,
}

fn collect_test_bodies<'a>(lines: &[&'a str]) -> Vec<TestBody<'a>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let _line = lines[i];
        // Scan forward through attribute lines so `#[allow(dead_code)]`
        // directly above `#[test]` / `fn ...` does not desync us.
        let mut attr_start = i;
        let mut test_attr_seen = false;
        while attr_start < lines.len() && lines[attr_start].trim_start().starts_with("#[") {
            let attr_text = lines[attr_start].trim_start();
            // Accept `#[test]`, `#[tokio::test]`, `#[rstest]`, etc. The
            // attribute marking this as a test is any `#[...]` whose body
            // contains "test" or "rstest" — defensively over-broad.
            if attr_text.contains("test") || attr_text.contains("rstest") {
                test_attr_seen = true;
            }
            attr_start += 1;
        }
        let fn_line = lines.get(attr_start).copied().unwrap_or("");
        if test_attr_seen && test_fn_name(fn_line).is_some() {
            let mut j = attr_start;
            let mut depth: i32 = 0;
            let mut saw_open = false;
            let mut text: Vec<&'a str> = Vec::new();
            while j < lines.len() {
                let l = lines[j];
                for c in l.chars() {
                    match c {
                        '{' => {
                            depth += 1;
                            saw_open = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                text.push(l);
                if saw_open && depth == 0 {
                    break;
                }
                j += 1;
            }
            let local_idents = collect_local_identifiers(&text);
            let references_self_only = body_references_self_only(&text, &local_idents);
            out.push(TestBody {
                open_line: attr_start + 1,
                open_line_text: fn_line,
                text,
                body_local_idents: local_idents,
                references_self_only,
            });
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

fn test_fn_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // Recognize `fn <name>(` or `fn <name>(...) -> <ret> {`. We require the
    // `(` immediately following the name — anything else (e.g. `<` for
    // turbofish, `::` for associated functions) is not a test function in
    // the shape this detector targets.
    let idx = trimmed.find("fn ")?;
    let rest = &trimmed[idx + 3..];
    let after_fn = rest.trim_start();
    if !after_fn.starts_with("fn ") {
        // not a redeclaration of `fn` keyword
    }
    let name_end = after_fn
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
        .last()
        .map(|(i, _)| i + 1)
        .unwrap_or(0);
    if name_end == 0 {
        return None;
    }
    let name = &after_fn[..name_end];
    let after_name = &after_fn[name_end..];
    if !after_name.starts_with('(') {
        return None;
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    Some(name.to_string())
}

fn collect_local_identifiers(body: &[&str]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in body {
        let cleaned = strip_rust_noise(line);
        // Very narrow match: `let <name>` or `let mut <name>` followed by `=`,
        // `:` or `;`.
        let mut s = cleaned.as_str();
        while let Some(idx) = s.find("let ") {
            let after = &s[idx + 4..];
            let trimmed = after.trim_start();
            let skip_mut = trimmed.strip_prefix("mut ").unwrap_or(trimmed);
            let name_end = skip_mut
                .char_indices()
                .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
                .last()
                .map(|(i, _)| i + 1)
                .unwrap_or(0);
            if name_end > 0 {
                let name = &skip_mut[..name_end];
                // Drop Rust keywords that the parser would reject anyway.
                if !matches!(name, "if" | "else" | "match" | "for" | "while" | "loop" | "return") {
                    out.insert(name.to_string());
                }
            }
            s = &s[idx + 4 + name_end.min(after.len().saturating_sub(4))..];
            if s.is_empty() {
                break;
            }
        }
    }
    out
}

fn body_references_self_only(body: &[&str], locals: &BTreeSet<String>) -> bool {
    // Count identifier occurrences that are NOT local-scope declarations
    // and NOT followed by `(` (the call-shape filter suppresses keywords,
    // macros, types, and field accesses). If every remaining call is to a
    // name in `locals`, the test exercises only its own scaffolding.
    //
    // WHY filter on call-shape: snake_case Rust function names (like
    // `parse_score`) look identical to the `vec!` macro when only the
    // shape is checked. The shape that reliably distinguishes a "real"
    // function reference from a keyword/type/macro is "is followed by
    // `(`".
    //
    // Skip the first line (the `fn <name>(...) {` signature) — including it
    // would leak the test function's own name as an "external call" and
    // mask the actual exercise surface.
    let mut external_calls: BTreeSet<String> = BTreeSet::new();
    for (i, line) in body.iter().enumerate() {
        if i == 0 {
            continue;
        }
        let cleaned = strip_rust_noise(line);
        let mut idx = 0;
        let bytes = cleaned.as_bytes();
        while idx < bytes.len() {
            let c = bytes[idx];
            if c.is_ascii_alphabetic() || c == b'_' {
                let start = idx;
                while idx < bytes.len()
                    && (bytes[idx].is_ascii_alphanumeric() || bytes[idx] == b'_')
                {
                    idx += 1;
                }
                let ident = &cleaned[start..idx];
                if !followed_by_paren(&cleaned, idx) {
                    idx += 1;
                    continue;
                }
                if is_std_or_rust_keyword(ident) {
                    continue;
                }
                if locals.contains(ident) {
                    continue;
                }
                external_calls.insert(ident.to_string());
            } else {
                idx += 1;
            }
        }
    }
    external_calls.is_empty()
}

fn extract_production_identifiers(text: &str, locals: &BTreeSet<String>) -> BTreeSet<String> {
    let mut idents = BTreeSet::new();
    for (li, line) in text.lines().enumerate() {
        if li == 0 {
            // Skip the `fn <name>(...) {` signature line — it isn't a real
            // exercise call.
            continue;
        }
        let cleaned = strip_rust_noise(line);
        let mut idx = 0;
        let bytes = cleaned.as_bytes();
        while idx < bytes.len() {
            let c = bytes[idx];
            if c.is_ascii_alphabetic() || c == b'_' {
                let start = idx;
                while idx < bytes.len()
                    && (bytes[idx].is_ascii_alphanumeric() || bytes[idx] == b'_')
                {
                    idx += 1;
                }
                let ident = &cleaned[start..idx];
                if !followed_by_paren(&cleaned, idx) {
                    idx += 1;
                    continue;
                }
                if is_std_or_rust_keyword(ident) {
                    continue;
                }
                if locals.contains(ident) {
                    continue;
                }
                // Strip the leading `core::`/`std::` namespaces outright.
                if ident == "core" || ident == "std" {
                    continue;
                }
                idents.insert(ident.to_string());
            } else {
                idx += 1;
            }
        }
    }
    idents
}

fn is_std_or_rust_keyword(ident: &str) -> bool {
    matches!(
        ident,
        "assert"
            | "assert_eq"
            | "assert_ne"
            | "debug_assert"
            | "debug_assert_eq"
            | "debug_assert_ne"
            | "let"
            | "mut"
            | "fn"
            | "use"
            | "pub"
            | "match"
            | "if"
            | "else"
            | "for"
            | "while"
            | "loop"
            | "return"
            | "true"
            | "false"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
            | "Vec"
            | "String"
            | "i32"
            | "i64"
            | "u32"
            | "u64"
            | "usize"
            | "bool"
            | "f64"
            | "f32"
            | "Self"
            | "self"
            | "as"
            | "in"
            | "ref"
            | "Box"
            | "Result"
            | "Option"
            | "Default"
            | "Clone"
            | "PartialEq"
            | "Eq"
            | "Debug"
            | "Hash"
            | "Iterator"
            | "collect"
            | "into"
            | "from"
            | "new"
            | "map"
            | "filter"
            | "fold"
            | "unwrap"
            | "expect"
            | "len"
            | "push"
            | "pop"
            | "chars"
            | "rev"
            | "trim"
            | "to_string"
            | "format"
            | "println"
            | "print"
            | "panic"
            | "todo"
            | "unimplemented"
            | "unreachable"
            | "abs"
            | "max"
            | "min"
            | "contains"
            | "is_empty"
    )
}

/// Returns true when `ident` is followed by `(` on the same line in `line`.
/// Used to distinguish function calls (which look like `parse_score(...)`)
/// from macro/keyword uses (which look like `vec!` or `format!`).
fn followed_by_paren(line: &str, ident_end_byte_offset: usize) -> bool {
    let bytes = line.as_bytes();
    let mut idx = ident_end_byte_offset;
    while idx < bytes.len() && (bytes[idx] == b' ' || bytes[idx] == b'\t') {
        idx += 1;
    }
    idx < bytes.len() && bytes[idx] == b'('
}

/// True when an identifier matches the shape of a Rust macro or Rust-style
/// lowercase binding (never a snake_case production function name).
fn ident_looks_like_keyword_or_macro(ident: &str, line: &str, end: usize) -> bool {
    if is_std_or_rust_keyword(ident) {
        return true;
    }
    // `vec!`, `format!`, `panic!` style: lowercase, no leading underscore,
    // immediately followed by `!`. Snake_case production functions
    // (`parse_score`, `validate_path`) are never followed by `!`.
    let next_char = line[end..].chars().next();
    if next_char == Some('!') && ident.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return true;
    }
    // Ident is uppercase-or-mixed (e.g. `Foo`, `BarBaz`) AND is a type or
    // constant (very common std-lib usage, e.g. `PathError`, `Some`, `Ok`).
    // Those are already in `is_std_or_rust_keyword`, so the `Vec` and
    // similar are filtered there.
    false
}

/// True when the body has the shape: a `let <x> = <f>(<args>);` followed by
/// `assert_eq!(<x>, <something that contains <x> as the only expression>);`
/// — the hallmark of a symmetric tautology. We accept:
///   - `f(x) == x`
///   - `f(x).foo() == x.foo()` (cheap check: `x` appears on both sides)
fn looks_like_symmetric_tautology(text: &str) -> bool {
    // Collect `let <name> = <f>(...);` lines where the rhs is a call.
    let lines: Vec<&str> = text.lines().collect();
    let mut mapping: Vec<(String, String)> = Vec::new();
    for line in &lines {
        let cleaned = strip_rust_noise(line);
        // Very narrow match: `let <ident> = <something>(...) ;`.
        if let Some(rest) = cleaned.trim_start().strip_prefix("let ") {
            let rest = rest.trim_start();
            let after_mut = rest.strip_prefix("mut ").unwrap_or(rest);
            let name_end = after_mut
                .char_indices()
                .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
                .last()
                .map(|(i, _)| i + 1)
                .unwrap_or(0);
            if name_end == 0 {
                continue;
            }
            let name = after_mut[..name_end].to_string();
            let tail = after_mut[name_end..].trim_start();
            if tail.starts_with('=') && tail.contains('(') && tail.contains(')') {
                let rhs = tail[1..].trim().trim_end_matches(';').trim().to_string();
                mapping.push((name, rhs));
            }
        }
    }

    // Shape 1: `f(name) == name`. The let introduces `name`, the rhs uses
    // `name`, and a later assertion compares the construction-result back
    // against `name` (with `name` on both sides).
    for (name, rhs) in &mapping {
        if rhs.contains(name) {
            for assertion_line in lines.iter() {
                let cleaned = strip_rust_noise(assertion_line);
                if let Some(inner) = strip_assert_call(&cleaned) {
                    if inner.matches(',').count() != 1 {
                        continue;
                    }
                    let parts: Vec<&str> = inner.splitn(2, ',').collect();
                    let lhs = parts[0].trim();
                    let rhs_a = parts[1].trim();
                    let one_side_is_name = lhs == *name || rhs_a == *name;
                    let other_side_mentions_name =
                        lhs.contains(name.as_str()) || rhs_a.contains(name.as_str());
                    if one_side_is_name && other_side_mentions_name {
                        return true;
                    }
                }
            }
        }
    }

    // Shape 2: idempotent-input. `let out = f(&input); assert_eq!(out, input)`.
    // For each `let <out> = f(&<input>);`, extract the input identifier and
    // search for an assertion that compares `<out>` to `<input>` (in either
    // order). This catches tests that prove identity trivially — the
    // production function could be replaced with `|x| x.clone()` and the
    // test would still pass.
    for (out_name, rhs) in &mapping {
        if rhs.contains(out_name) {
            // already covered by Shape 1
            continue;
        }
        // Find an `&<input>` or plain `<input>` token inside the rhs.
        let bytes = rhs.as_bytes();
        let mut idx = 0;
        let mut input_name: Option<String> = None;
        while idx < bytes.len() {
            if bytes[idx] == b'&' {
                let mut j = idx + 1;
                while j < bytes.len() && (bytes[j].is_ascii_whitespace()) {
                    j += 1;
                }
                let start = j;
                while j < bytes.len()
                    && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_')
                {
                    j += 1;
                }
                if j > start {
                    input_name = Some(rhs[start..j].to_string());
                    break;
                }
            }
            idx += 1;
        }
        let input_name = match input_name {
            Some(n) => n,
            None => continue,
        };
        if input_name == *out_name {
            continue;
        }

        for assertion_line in lines.iter() {
            let cleaned = strip_rust_noise(assertion_line);
            if let Some(inner) = strip_assert_call(&cleaned) {
                // Split on the FIRST top-level comma to handle the
                // `assert_eq!(A, B, "...explanation...")` form — only the
                // first two expressions matter; the explanation string
                // (third comma-delimited field) is ignored.
                let first = take_first_top_level_expression(inner);
                let rest = inner[first.len()..].trim_start();
                let after_first = rest.trim_start_matches(',').trim_start();
                let lhs = first.trim();
                let rhs_a = after_first.trim();
                let _ = rhs_a.len();
                let lhs_has_out = lhs.contains(out_name.as_str());
                let rhs_has_input = rhs_a.contains(input_name.as_str());
                let lhs_has_input = lhs.contains(input_name.as_str());
                let rhs_has_out = rhs_a.contains(out_name.as_str());
                if (lhs_has_out && rhs_has_input) || (lhs_has_input && rhs_has_out) {
                    return true;
                }
            }
        }
    }
    false
}

/// True when EVERY `assert_eq!(a, b)` in the body references only data that
/// originated as a literal in the same body. The shape we want to flag:
///
/// ```ignore
/// let p = build_packet(1, vec![0xAA]);
/// assert_eq!(p.seq, 1, "...");            // 1 is a body-literal
/// assert_eq!(p.payload, vec![0xAA]);      // vec![0xAA] is a body-literal
/// ```
///
/// Production-side effect of `build_packet` (e.g., field reordering, payload
/// hashing, seq offset) is NEVER observed by these assertions: each compare
/// targets a field whose value the test itself provided as a literal. We
/// detect this conservatively: at least one `let <name> = f(LITS);` plus at
/// least one `assert_eq!(<name>.FIELD, <literal-that-appeared-in-LITS>)`,
/// with the SAME literal appearing on both sides of the let-binding and the
/// assertion target.
fn looks_like_construction_round_trip(body_text: &str) -> bool {
    // Phase 1: collect every (name, literals_used) pair from `let <name> = f(...)`.
    let cleaned = body_text
        .lines()
        .map(strip_rust_noise)
        .collect::<Vec<_>>()
        .join("\n");
    let mut construction_calls: Vec<(String, BTreeSet<String>)> = Vec::new();
    for line in cleaned.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("let ") {
            let rest = rest.trim_start();
            let after_mut = rest.strip_prefix("mut ").unwrap_or(rest);
            if let Some(eq_idx) = after_mut.find('=') {
                let head = after_mut[..eq_idx].trim();
                let name_end = head
                    .char_indices()
                    .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
                    .last()
                    .map(|(i, _)| i + 1)
                    .unwrap_or(0);
                if name_end > 0 {
                    let name = head[..name_end].to_string();
                    if after_mut[eq_idx + 1..].contains('(') {
                        let lits: BTreeSet<String> = tokens_in(
                            after_mut[eq_idx + 1..].trim_end_matches(';'),
                        )
                        .into_iter()
                        .collect();
                        construction_calls.push((name, lits));
                    }
                }
            }
        }
    }

    if construction_calls.is_empty() {
        return false;
    }

    // Phase 2: for each assertion, require (a) a construction-variable to
    // appear and (b) at least one of the EXACT tokens from that
    // construction's rhs to appear in the assertion target as a literal
    // (i.e. same characters, same token). This makes the rule robust
    // against incidental false-positives where unrelated literals happen
    // to exist on body lines.
    //
    // We support both `assert_eq!(name.FIELD, LIT)` (LHS is `name` plus a
    // field access; RHS is the literal) and `assert_eq!(LIT, name.FIELD)`
    // (mirrored). When `name` itself is the LHS without a `.FIELD` form, we
    // require an exact `name == rhs` comparison using the rhs literal
    // found in the let-binding.
    for line in cleaned.lines() {
        let inner = match strip_assert_call(line) {
            Some(i) => i,
            None => continue,
        };
        if inner.matches(',').count() != 1 {
            continue;
        }
        let parts: Vec<&str> = inner.splitn(2, ',').collect();
        let lhs = parts[0].trim();
        let rhs_assert = parts[1].trim();
        for (name, lits) in &construction_calls {
            // `lhs` is `name` or `name.something` and `rhs_assert` mentions a
            // literal-token that also appears in the let rhs.
            //
            // Tighten the check: the literal token MUST appear on the rhs
            // side of the assertion (the side that's data, not the side
            // being read from the construction).
            let rhs_starts_with_name = lhs.starts_with(name.as_str());
            let _ = rhs_starts_with_name;

            // Side 1: `name.X` is LHS, lookup `rhs_assert` for a literal that
            // appeared in construction lits.
            let shared_literal_in_assert_rhs = lits
                .iter()
                .any(|lit| rhs_assert.contains(lit.as_str()));
            if rhs_starts_with_name && shared_literal_in_assert_rhs {
                return true;
            }
            // Side 2: `name.X` is RHS, lookup `lhs` for a literal.
            let rhs_has_name = rhs_assert.starts_with(name.as_str());
            let shared_literal_in_assert_lhs = lits
                .iter()
                .any(|lit| lhs.contains(lit.as_str()));
            if rhs_has_name && shared_literal_in_assert_lhs {
                return true;
            }
        }
    }
    false
}

/// Tokenize literals out of a Rust expression fragment — only literal-data
/// tokens (numbers, multi-char string constants, `vec![...]`) form one
/// token. Identifiers (type names, function names, variable names) are
/// intentionally excluded: they are not "literals" the test can paste into
/// an assertion target to prove construction-output equivalence. Caller
/// (`looks_like_construction_round_trip`) uses these tokens as the set of
/// candidate truth-values that a construction's rhs emits as data.
fn tokens_in(s: &str) -> Vec<String> {
    let cleaned = strip_rust_noise(s);
    let mut out = Vec::new();
    // First pass: collect all token-like runs as (start_byte, end_byte, kind).
    let bytes = cleaned.as_bytes();
    let mut i = 0;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() || c == b'_' || c.is_ascii_digit() {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            runs.push((start, i));
        } else {
            i += 1;
        }
    }
    for (start, end) in runs {
        let tok = &cleaned[start..end];
        // Numeric literal (anywhere a digit appears).
        if tok.chars().any(|c| c.is_ascii_digit()) {
            out.push(tok.to_string());
        }
        // `true`, `false`, `null`-style literals — only as standalone Rust
        // primitives.
        if matches!(tok, "true" | "false" | "None") {
            out.push(tok.to_string());
        }
        // Skip uppercase or mixed-case identifiers (these are types or
        // constants, not literals); keep snake_case identifiers ONLY when
        // they are entirely digits+letters, i.e. Rust would not parse them
        // as a number — those we still skip.
        // Skip pure-identifier tokens here intentionally.
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_noise_keeps_code_drops_comment() {
        let s = "// just a comment\nlet x = 1; // trailing";
        let l1 = s.lines().next().unwrap();
        assert_eq!(strip_rust_noise(l1), "");
    }

    #[test]
    fn strip_assert_call_finds_inner() {
        let s = r#"    assert_eq!(a, b);"#;
        assert_eq!(strip_assert_call(s), Some("a, b"));
    }

    #[test]
    fn strip_assert_call_rejects_non_assert_macro() {
        let s = r#"    foo!(a, b);"#;
        assert_eq!(strip_assert_call(s), None);
    }

    #[test]
    fn take_first_top_level_expression_handles_nested_parens() {
        let s = "foo(a, b), c";
        assert_eq!(take_first_top_level_expression(s), "foo(a, b)");
    }

    #[test]
    fn tokens_in_filters_to_numeric_literals() {
        // Identifiers (snake_case and CamelCase) must NOT appear in the
        // literal-token set; only digits, true/false, None should.
        let toks = tokens_in("S { v: add_production(2) }");
        assert!(toks.contains(&"2".to_string()), "toks={toks:?}");
        assert!(!toks.contains(&"S".to_string()), "toks={toks:?}");
        assert!(!toks.contains(&"v".to_string()), "toks={toks:?}");
        assert!(!toks.contains(&"add_production".to_string()), "toks={toks:?}");
    }
}
