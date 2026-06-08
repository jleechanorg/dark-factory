#!/usr/bin/env python3
"""Render Excalidraw JSON to PNG using a LOCAL copy of @excalidraw/excalidraw.

The bundled `render_excalidraw.py` from the skill loads the library from
esm.sh, which the Playwright Chromium sandbox cannot reach. This wrapper:

  1. Pre-bundles the Excalidraw prod entry into a single ESM file with esbuild,
     inlining all transitive deps (react, jotai, roughjs, etc.) so the
     browser only needs to fetch one script.
  2. Starts a tiny HTTP server on a free port, serving the bundle and any
     companion assets (fonts, locales) from the skill's node_modules/.
  3. Renders each .excalidraw file to PNG via Playwright headless Chromium.

Usage:
    python diagrams/render_local.py diagrams/excalidraw/<name>.excalidraw \
        --output diagrams/png/<name>.png --scale 2 --width 1800
"""
from __future__ import annotations

import argparse
import contextlib
import http.server
import json
import os
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

SKILL_REF = Path.home() / ".claude/skills/excalidraw-diagram/references"
NODE_MODULES = SKILL_REF / "node_modules"
EXCALIDRAW_ENTRY = NODE_MODULES / "@excalidraw/excalidraw/dist/prod/index.js"

# A cache of pre-bundled Excalidraw ESM, regenerated only if the entry mtime changes.
_BUNDLE_CACHE = Path(tempfile.gettempdir()) / "dark-factory-excalidraw.bundle.js"

# A separate directory for companion assets (fonts, locales) that Excalidraw
# fetches at runtime. We copy them out of node_modules/@excalidraw/excalidraw/dist
# the first time the server starts.
_ASSETS_DIR = Path(tempfile.gettempdir()) / "dark-factory-excalidraw-assets"

_ESBUILD = (
    "/Users/jleechan/project_agento/agent-orchestrator/node_modules/.pnpm"
    "/esbuild@0.27.3/node_modules/esbuild/bin/esbuild"
)


def _build_bundle_if_stale() -> Path:
    """Run esbuild to produce a single ESM bundle of Excalidraw + all deps."""
    _BUNDLE_CACHE.parent.mkdir(parents=True, exist_ok=True)
    src_mtime = EXCALIDRAW_ENTRY.stat().st_mtime
    if _BUNDLE_CACHE.exists() and _BUNDLE_CACHE.stat().st_mtime > src_mtime:
        return _BUNDLE_CACHE
    if not Path(_ESBUILD).exists():
        print(f"ERROR: esbuild not found at {_ESBUILD}", file=sys.stderr)
        sys.exit(2)
    print(f"[bundle] esbuild --bundle -> {_BUNDLE_CACHE}", file=sys.stderr)
    subprocess.run(
        [
            _ESBUILD,
            "--bundle",
            "--format=esm",
            "--target=chrome120",
            f"--outfile={_BUNDLE_CACHE}",
            "--legal-comments=none",
            str(EXCALIDRAW_ENTRY),
        ],
        check=True,
        cwd=NODE_MODULES,
    )
    return _BUNDLE_CACHE


def _sync_assets() -> None:
    """Copy the Excalidraw dist companion assets (fonts, locales) to a stable dir."""
    src = NODE_MODULES / "@excalidraw/excalidraw/dist"
    if _ASSETS_DIR.exists():
        shutil.rmtree(_ASSETS_DIR)
    _ASSETS_DIR.mkdir(parents=True, exist_ok=True)
    for sub in ("fonts", "locales", "data"):
        s = src / sub
        if s.is_dir():
            shutil.copytree(s, _ASSETS_DIR / sub)


_RENDER_HTML = """<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8" />
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body { background: #ffffff; overflow: hidden; }
    #root { display: inline-block; }
    #root svg { display: block; }
  </style>
</head>
<body>
  <div id="root"></div>
  <script type="module">
    import * as ExcalidrawLib from "/excalidraw.bundle.js";
    const { exportToSvg } = ExcalidrawLib;

    window.renderDiagram = async function(jsonData) {
      try {
        const data = typeof jsonData === "string" ? JSON.parse(jsonData) : jsonData;
        const elements = data.elements || [];
        const appState = data.appState || {};
        const files = data.files || {};

        appState.viewBackgroundColor = appState.viewBackgroundColor || "#ffffff";
        appState.exportWithDarkMode = false;

        const svg = await exportToSvg({
          elements: elements,
          appState: { ...appState, exportBackground: true },
          files: files,
        });

        const root = document.getElementById("root");
        root.innerHTML = "";
        root.appendChild(svg);

        window.__renderComplete = true;
        window.__renderError = null;
        return { success: true };
      } catch (err) {
        window.__renderComplete = true;
        window.__renderError = err.message;
        return { success: false, error: err.message };
      }
    };

    window.__moduleReady = true;
  </script>
</body>
</html>
"""


def find_free_port() -> int:
    import socket
    for _ in range(20):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            try:
                s.bind(("127.0.0.1", port))
                return port
            except OSError:
                continue
    raise RuntimeError("could not find a free port after 20 tries")


class _Handler(http.server.SimpleHTTPRequestHandler):
    """Serves the pre-bundled Excalidraw + companion assets + a render template."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(NODE_MODULES), **kwargs)

    def end_headers(self):
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, format, *args):
        # quiet
        pass

    def do_GET(self):
        if self.path == "/__render.html" or self.path.startswith("/__render.html"):
            payload = _RENDER_HTML.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        if self.path == "/excalidraw.bundle.js" or self.path.startswith("/excalidraw.bundle.js"):
            payload = _BUNDLE_CACHE.read_bytes()
            self.send_response(200)
            self.send_header("Content-Type", "application/javascript; charset=utf-8")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        # Companion assets (fonts, locales, data) served from a stable dir
        stripped = self.path.lstrip("/")
        candidate = _ASSETS_DIR / stripped
        if candidate.is_file():
            payload = candidate.read_bytes()
            self.send_response(200)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        return super().do_GET()


@contextlib.contextmanager
def local_server(port: int):
    socketserver.TCPServer.allow_reuse_address = True
    httpd = socketserver.TCPServer(("127.0.0.1", port), _Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield httpd
    finally:
        httpd.shutdown()
        httpd.server_close()


def render(excalidraw_path: Path, output_path: Path, scale: int, width: int):
    from playwright.sync_api import sync_playwright

    if not NODE_MODULES.exists():
        print(f"ERROR: {NODE_MODULES} not found. Run: "
              f"npm install --prefix {SKILL_REF} @excalidraw/excalidraw",
              file=sys.stderr)
        sys.exit(2)

    bundle = _build_bundle_if_stale()
    _sync_assets()

    data = json.loads(excalidraw_path.read_text())
    elements = data.get("elements", [])
    if not elements:
        print(f"ERROR: {excalidraw_path} has no elements", file=sys.stderr)
        sys.exit(2)

    # Compute bounding box
    min_x = min_y = float("inf")
    max_x = max_y = float("-inf")
    for el in elements:
        if el.get("isDeleted"):
            continue
        x = el.get("x", 0)
        y = el.get("y", 0)
        w = el.get("width", 0)
        h = el.get("height", 0)
        min_x = min(min_x, x)
        min_y = min(min_y, y)
        max_x = max(max_x, x + w)
        max_y = max(max_y, y + h)
        if el.get("type") in ("arrow", "line") and "points" in el:
            for px, py in el["points"]:
                min_x = min(min_x, x + px)
                min_y = min(min_y, y + py)
                max_x = max(max_x, x + px)
                max_y = max(max_y, y + py)

    pad = 40
    canvas_w = max(int(max_x - min_x + pad * 2), 400)
    canvas_h = max(int(max_y - min_y + pad * 2), 300)

    port = find_free_port()
    with local_server(port):
        with sync_playwright() as p:
            browser = p.chromium.launch(headless=True)
            ctx = browser.new_context(viewport={"width": width, "height": 1200})
            page = ctx.new_page()
            page.on("console", lambda m: print(f"  [console.{m.type}] {m.text}"))
            page.on("pageerror", lambda e: print(f"  [pageerror] {e}"))
            page.goto(f"http://127.0.0.1:{port}/__render.html")
            page.wait_for_function("window.__moduleReady === true", timeout=60000)
            page.evaluate("(data) => window.renderDiagram(data)", json.dumps(data))
            page.wait_for_function("window.__renderComplete === true", timeout=15000)
            err = page.evaluate("window.__renderError")
            if err:
                print(f"ERROR: render error: {err}", file=sys.stderr)
                sys.exit(3)
            svg = page.locator("#root svg")
            svg.screenshot(path=str(output_path), scale="device")
            browser.close()

    print(f"wrote {output_path}  ({canvas_w}x{canvas_h} canvas, {bundle.stat().st_size//1024}kb bundle)")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("input", type=Path)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--scale", type=int, default=2)
    p.add_argument("--width", type=int, default=1800)
    args = p.parse_args()
    render(args.input, args.output, args.scale, args.width)


if __name__ == "__main__":
    main()
