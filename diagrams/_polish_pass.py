"""Final polish pass: fix specific cramping issues that the bulk resize pass
left behind.

  - Diagram 1: widen the LLM-layer content box to fit the gateway text
  - Diagram 2: push the right cluster (diamond + 3 outcomes) further right
    so the center "append step" box and the diamond have ~80px gap
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

EXCAL_DIR = Path(__file__).parent / "excalidraw"


def polish_diag1() -> None:
    p = EXCAL_DIR / "01-3-layer-convergence.excalidraw"
    data = json.loads(p.read_text())
    elements = data["elements"]

    # Widen the LLM content box (text "OpenClaw gateway · wafer proxy · thinclaw MCP")
    # Old: x=550 y=520 w=240 h=50
    # New: x=400 y=520 w=480 h=50 — center the wider box on the LLM layer
    target_text = "OpenClaw gateway"
    for el in elements:
        t = (el.get("text") or "")
        if target_text in t:
            old_x = el["x"]
            el["x"] = 400
            el["width"] = 480
            print(f"  LLM text: x {int(old_x)}->400, w 240->480", file=sys.stderr)
        # Match the parent rectangle (same x,y,w,h)
        if el.get("type") == "rectangle" and 540 <= el.get("x", 0) <= 560 and 510 <= el.get("y", 0) <= 530:
            el["x"] = 400
            el["width"] = 480
            print(f"  LLM rect: x 550->400, w 240->480", file=sys.stderr)
    p.write_text(json.dumps(data, indent=2))
    print(f"  wrote {p}", file=sys.stderr)


def polish_diag2() -> None:
    p = EXCAL_DIR / "02-pipeline-execution.excalidraw"
    data = json.loads(p.read_text())
    elements = data["elements"]

    # The "decide next node" diamond is the rightmost diamond (x=730, y=410).
    # The leftmost diamond (x=180, y=90) is the "resolve(node)" decision — keep it.
    diamonds = [el for el in elements if el.get("type") == "diamond"]
    right_diamond = max(diamonds, key=lambda el: el["x"])
    x_min = right_diamond["x"] - 20
    moved = 0
    for el in elements:
        if el.get("x", 0) >= x_min:
            el["x"] = el.get("x", 0) + 220
            moved += 1
    print(f"  shifted right cluster (x>={int(x_min)}) by +220: {moved} elements", file=sys.stderr)

    p.write_text(json.dumps(data, indent=2))
    print(f"  wrote {p}", file=sys.stderr)


def main() -> None:
    print("== polish diag1 ==", file=sys.stderr)
    polish_diag1()
    print("== polish diag2 ==", file=sys.stderr)
    polish_diag2()


if __name__ == "__main__":
    main()
