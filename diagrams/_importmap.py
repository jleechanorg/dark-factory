"""Build a complete importmap for the Excalidraw prod bundle.

The Excalidraw ESM bundle has many transitive bare imports (`react`,
`jotai`, `roughjs/bin/rough`, etc.) that the browser must resolve to
local files in `node_modules/`. We scan the entry, collect every bare
specifier, then for each one look at the package's `package.json` and
emit an import-map entry pointing at the resolved file under our local
HTTP server.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Dict

NODE_MODULES = Path.home() / ".claude/skills/excalidraw-diagram/references/node_modules"

# Match `from"..."` and `import"..."` for bare specifiers.
# (The bundle uses `import` for side-effect imports too.)
_SPEC_RE = re.compile(r'''(?:from|import)\s*["']([^"']+)["']''')


def _split_pkg(spec: str) -> tuple[str, str]:
    """Split a bare specifier into (pkg, rest). Handles @scope/name."""
    if spec.startswith("@"):
        parts = spec.split("/", 2)
        if len(parts) >= 2:
            return parts[0] + "/" + parts[1], parts[2] if len(parts) == 3 else ""
    parts = spec.split("/", 1)
    return parts[0], parts[1] if len(parts) == 2 else ""


def _resolve_bare(spec: str) -> str:
    """Map a bare specifier to a path under node_modules, relative to NODE_MODULES."""
    pkg, rest = _split_pkg(spec)
    pkg_path = NODE_MODULES / pkg
    if rest:
        # Try as a file
        candidate = pkg_path / rest
        if candidate.is_file():
            return f"{pkg}/{rest}"
        # Try as a file with .js appended (CommonJS modules without explicit extension)
        candidate_js = pkg_path / (rest + ".js")
        if candidate_js.is_file():
            return f"{pkg}/{rest}.js"
        # Try package.json exports for the subpath
        pkg_json = _read_pkg(pkg_path)
        if pkg_json and "exports" in pkg_json:
            sub = rest if rest.startswith("./") else f"./{rest}"
            resolved = _resolve_exports(pkg_json["exports"], sub)
            if resolved:
                return f"{pkg}/{_with_js_if_needed(resolved, pkg_path)}"

    # No subpath — use package.json main/module/exports
    pkg_json = _read_pkg(pkg_path)
    if pkg_json is None:
        raise FileNotFoundError(f"package.json not found for {spec!r} at {pkg_path}")
    if "exports" in pkg_json:
        resolved = _resolve_exports(pkg_json["exports"], ".")
        if resolved:
            return f"{pkg}/{_with_js_if_needed(resolved, pkg_path)}"
    main = pkg_json.get("module") or pkg_json.get("main") or "index.js"
    return f"{pkg}/{_with_js_if_needed(_clean(main), pkg_path)}"


def _with_js_if_needed(rel_path: str, pkg_path: Path) -> str:
    """Append `.js` to a path if it has no extension and the .js variant exists."""
    p = pkg_path / rel_path
    if p.is_file():
        return rel_path
    if "." not in rel_path.rsplit("/", 1)[-1]:
        # No extension — try adding .js
        if (pkg_path / (rel_path + ".js")).is_file():
            return rel_path + ".js"
    return rel_path


def _clean(p: str) -> str:
    """Strip a leading './' from an exports value."""
    return p[2:] if p.startswith("./") else p


def _resolve_exports(exports, subpath: str) -> str | None:
    """Walk a package.json "exports" field and resolve `subpath` (e.g. "." or "./foo")."""
    if isinstance(exports, str):
        return _clean(exports) if subpath == "." else None
    if isinstance(exports, dict):
        if subpath in exports:
            v = exports[subpath]
            if isinstance(v, str):
                return _clean(v)
            if isinstance(v, dict):
                # Prefer "import" condition for ESM, fall back to "default"
                for cond in ("import", "module", "browser", "default"):
                    if cond in v and isinstance(v[cond], str):
                        return _clean(v[cond])
        # Maybe subpath is "./foo" — try the dict form
        key = subpath if subpath.startswith("./") else f"./{subpath}"
        if key in exports:
            v = exports[key]
            if isinstance(v, str):
                return _clean(v)
            if isinstance(v, dict):
                for cond in ("import", "module", "browser", "default"):
                    if cond in v and isinstance(v[cond], str):
                        return _clean(v[cond])
    if isinstance(exports, list):
        for e in exports:
            r = _resolve_exports(e, subpath)
            if r is not None:
                return r
    return None


def _read_pkg(pkg_path: Path) -> dict | None:
    pj = pkg_path / "package.json"
    if not pj.is_file():
        return None
    try:
        return json.loads(pj.read_text())
    except Exception:
        return None


def build_importmap(entry_path: Path) -> Dict[str, str]:
    """Read `entry_path`, find every bare specifier, resolve each, and return the importmap dict."""
    text = entry_path.read_text()
    specifiers = set()
    for m in _SPEC_RE.finditer(text):
        s = m.group(1)
        # Skip absolute paths and relative paths
        if s.startswith(("/", "./", "../")):
            continue
        specifiers.add(s)
    print(f"[importmap] found {len(specifiers)} bare specifiers", file=sys.stderr)
    mapping: Dict[str, str] = {}
    for spec in sorted(specifiers):
        try:
            target = _resolve_bare(spec)
            full = NODE_MODULES / target
            if not full.is_file():
                print(f"[importmap] WARN: resolved {spec!r} -> {target} (NOT FOUND)", file=sys.stderr)
            # Prepend a leading slash for the URL served by NodeModulesHandler
            mapping[spec] = f"/{target}"
        except FileNotFoundError as e:
            print(f"[importmap] WARN: {e}", file=sys.stderr)
    return mapping


if __name__ == "__main__":
    import argparse
    p = argparse.ArgumentParser()
    p.add_argument("entry", type=Path)
    args = p.parse_args()
    print(json.dumps({"imports": build_importmap(args.entry)}, indent=2))
