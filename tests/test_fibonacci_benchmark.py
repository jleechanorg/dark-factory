from __future__ import annotations

import json
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
BENCH = ROOT / "benchmarks" / "fibonacci"


def test_fibonacci_benchmark_scores_reference_candidate(tmp_path):
    candidate = tmp_path / "candidate"
    shutil.copytree(BENCH / "starter", candidate)
    (candidate / "fib.py").write_text(
        "from __future__ import annotations\n"
        "import sys\n\n"
        "def fib(n: int) -> int:\n"
        "    a, b = 0, 1\n"
        "    for _ in range(n):\n"
        "        a, b = b, a + b\n"
        "    return a\n\n"
        "def main() -> int:\n"
        "    if len(sys.argv) != 2:\n"
        "        print('usage: fib.py <n>', file=sys.stderr)\n"
        "        return 2\n"
        "    try:\n"
        "        n = int(sys.argv[1])\n"
        "    except ValueError:\n"
        "        print('n must be an integer', file=sys.stderr)\n"
        "        return 2\n"
        "    if n < 0:\n"
        "        print('n must be non-negative', file=sys.stderr)\n"
        "        return 2\n"
        "    print(fib(n))\n"
        "    return 0\n\n"
        "if __name__ == '__main__':\n"
        "    raise SystemExit(main())\n"
    )

    public = subprocess.run(
        ["bash", str(BENCH / "scripts" / "run_candidate.sh"), str(candidate)],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert public.returncode == 0, public.stdout + public.stderr

    score = subprocess.run(
        [sys.executable, str(BENCH / "scripts" / "score_candidate.py"), str(candidate)],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert score.returncode == 0, score.stdout + score.stderr
    payload = json.loads(score.stdout)
    assert payload == {
        "benchmark": "fibonacci",
        "status": "pass",
        "sealed": True,
        "passed": 8,
        "total": 8,
    }

