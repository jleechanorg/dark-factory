#!/usr/bin/env python3
from pathlib import Path

ROOT = Path('.')

exts = {'.js', '.html', '.css', '.json', '.md', '.rules'}
ignore_parts = {'node_modules', 'package-lock.json', 'results', '.git', 'dist', 'build'}


def count_lines(path: Path) -> int:
    if path.is_dir():
        return 0
    if any(p in path.parts for p in ignore_parts):
        return 0
    if path.suffix not in exts:
        return 0
    try:
        return sum(1 for line in path.read_text(errors='ignore').splitlines() if line.strip())
    except Exception:
        return 0

source_lines = 0
frontend_lines = 0
public_dir = ROOT / 'src' / 'public'

for p in ROOT.rglob('*'):
    c = count_lines(p)
    source_lines += c
    if public_dir in p.parents:
        frontend_lines += c

print(f"source_lines={source_lines}")
print(f"frontend_lines={frontend_lines}")

if source_lines < 5000:
    raise SystemExit(f"source_lines floor not met: {source_lines}")
if frontend_lines < 2000:
    raise SystemExit(f"frontend_lines floor not met: {frontend_lines}")
print('RESULT: PASS')
