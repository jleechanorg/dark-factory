# Fix: ${goal}

A sealed evaluator reported a failure on `df_demo3/roman.py`.

Use the evaluator output from the previous pipeline step: scenario names and
redacted detail buckets are the only diagnostic signal you should receive.
Do not look for evaluator source, hidden tests, or holdout files on disk.
If the failure points at conversion behaviour, fix the conversion logic while
staying within the visible spec.

Hard constraints:

- Touch only `df_demo3/roman.py` (or `df_demo3/__init__.py` if necessary).
- Do not open or push a pull request.

When done, briefly state what you changed.
