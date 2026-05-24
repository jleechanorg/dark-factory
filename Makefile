PYTHON ?= .venv/bin/python

.PHONY: build test conformance benchmark-smoke

build:
	$(PYTHON) -m compileall -q runner

test:
	$(PYTHON) -m pytest tests/ -q

conformance:
	$(PYTHON) bin/conformance score

benchmark-smoke:
	$(PYTHON) -m pytest tests/test_fibonacci_benchmark.py tests/test_benchmark_boundary.py -q
	benchmarks/amazon-clone/scripts/run_matrix_deterministic.sh /tmp/dark-factory-amazon-matrix-smoke >/tmp/dark-factory-amazon-matrix-smoke.json
