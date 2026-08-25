# Dark Factory Code Standards & Conventions

This document defines code standards and conventions for the `dark-factory` repository.

## 1. Anchor Comment Pattern & Language Syntax

Anchor comments are inserted into watched paths (such as `daemon/src/adapters.rs`) to trigger the Evidence Gate workflow and anchor verification evidence to a specific PR.

> [!IMPORTANT]
> **Rule:** Anchor comments must use the language's native comment syntax.
> - For **Rust** (`.rs`): use `//` (e.g. `// PR #<N>`)
> - For **Python** (`.py`): use `#` (e.g. `# PR #<N>`)
> - For **Shell** (`.sh`, `.bash`, `.zsh`): use `#` (e.g. `# PR #<N>`)
> - For **YAML** (`.yml`, `.yaml`): use `#` (e.g. `# PR #<N>`)
> - For **TOML** (`.toml`): use `#` (e.g. `# PR #<N>`)
> - For **SQL** (`.sql`): use `--` (e.g. `-- PR #<N>`)
> - For **JavaScript / TypeScript** (`.js`, `.mjs`, `.ts`): use `//` (e.g. `// PR #<N>`)
> - For **Markdown** (`.md`): use `<!-- PR #<N> -->` or a markdown section

### Root Cause / History
In PR #665 and PR #666, an anchor-commit push erroneously used `# PR #N` in `daemon/src/adapters.rs`, which broke Rust compilation (`cargo check` / `cargo build`) because `#` is invalid syntax outside attribute macros (`#[...]` / `#![...]`).

### Validation

Use the language's real parser, compiler, formatter, or linter for changed
source files. A cross-language line-regex is intentionally not provided: it
cannot reliably distinguish comments from multiline strings, block comments,
heredocs, or prose and would create false confidence. For the Rust incident
above, `cargo check --manifest-path daemon/Cargo.toml` is authoritative.

---

## 2. Commit Message & PR Standards
- All commits must follow conventional commits (`feat:`, `fix:`, `chore:`, `docs:`, `test:`, `refactor:`).
- When operating as an Antigravity agent, commits and PR titles must include the required `[antig]` prefix per repository guidelines.
- PRs must include the `## Evidence` section with a verifiable public gist URL bound to the exact head commit SHA:
  ```markdown
  **Evidence**: https://gist.github.com/<user>/<gist_id> (head <head_sha>)
  ```
- Fast-forward standard pushes only (`git push origin <branch>`). Never force-push over commits authored by other agents or operators.

---

## 3. Testing & TDD Standard
- All bug fixes and feature additions must follow the Test-Driven Development (TDD) red→green cycle:
  1. Write failing test reproducing the bug or gap (Red).
  2. Implement the minimal fix (Green).
  3. Refactor and verify full test suite remains clean.
- Unit tests and integration tests must run without network dependence.
