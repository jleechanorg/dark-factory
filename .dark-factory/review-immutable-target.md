Review only the immutable-target validation changes in runner/review_controller.py and tests/test_immutable_target.py. Verify:

1. workspace_path must be a non-symlink real directory; symlink/path-escape/non-existent/file-as-dir all fail closed.
2. workspace_path must not resolve under any holdout root; symlinks that resolve into holdouts fail closed.
3. evidence paths must be regular files inside workspace_path, not symlinks, not directories, not over 1 MiB; symlink-swap to escape workspace is rejected.
4. Post-review reverification (already in _verify_controller_workspace) catches head/tree/diff/evidence mutation.
5. No ZFC violation: command strings or prompt text are not regex-classified; the controller owns shape and digest invariants, the model owns evidence sufficiency.
6. End with the exact HEAD SHA and `VERDICT: PASS` or `VERDICT: FAIL`.