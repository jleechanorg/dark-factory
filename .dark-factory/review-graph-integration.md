Review only the graph-integration changes in runner/review_controller.py, runner/review_cli.py, runner/handler_parallel_reviewer.py, runner/handler_dispatch.py (if touched), pipelines/factory/{gates,level5_feature,pr_gates}.dot, and tests/test_graph_controller_integration.py. Verify:

1. The shared helper exists, both CLI and graph call it, and per-lane output dirs are distinct.
2. The three .dot pipelines still pass preflight and now opt into review_contract="cold-review-v1" on the reviewer nodes.
3. No ZFC violation: command strings or prompt text are not regex-classified.
4. The existing 111-test shard stays green.
5. End with exact HEAD SHA and `VERDICT: PASS` or `VERDICT: FAIL`.