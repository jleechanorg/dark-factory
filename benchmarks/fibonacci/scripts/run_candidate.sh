#!/usr/bin/env bash
set -euo pipefail

PYTHON="${PYTHON:-python3}"

candidate="${1:?candidate directory required}"
cd "$candidate"

"$PYTHON" fib.py 0 | grep -Fx '0'
"$PYTHON" fib.py 1 | grep -Fx '1'
"$PYTHON" fib.py 10 | grep -Fx '55'

if "$PYTHON" fib.py -1 >/tmp/fibonacci-negative.out 2>/tmp/fibonacci-negative.err; then
  echo "negative input unexpectedly succeeded" >&2
  exit 1
fi

if "$PYTHON" fib.py abc >/tmp/fibonacci-abc.out 2>/tmp/fibonacci-abc.err; then
  echo "non-integer input unexpectedly succeeded" >&2
  exit 1
fi

echo "public acceptance passed"


