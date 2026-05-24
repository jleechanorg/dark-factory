# Fibonacci CLI visible spec

Build a production-ready Fibonacci command-line tool in `fib.py`.

## User story

As a developer, I want to run a local CLI that returns the nth Fibonacci number
so that I can use it in scripts and simple automation.

## Interface

```bash
python fib.py <n>
```

## Requirements

- `n` is a non-negative integer.
- Output exactly one base-10 integer followed by a newline on success.
- Use the conventional sequence: `fib(0)=0`, `fib(1)=1`, `fib(2)=1`.
- Support at least `n=50` quickly without recursion-depth issues.
- On invalid input, exit non-zero and write a useful error to stderr.
- Do not call external services.
- Do not read benchmark evaluator files or hidden validation details.

## Public acceptance checks

- `python fib.py 0` prints `0`.
- `python fib.py 1` prints `1`.
- `python fib.py 10` prints `55`.
- `python fib.py -1` exits non-zero.
- `python fib.py abc` exits non-zero.

