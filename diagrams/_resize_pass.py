"""Resize cramped text elements + their parent rectangles in .excalidraw files.

Two passes:
  1. Bump 11pt → 14pt and 12pt → 14pt in narrow text boxes; widen parent rectangle
     proportionally to maintain padding.
  2. Diagram-specific: shift the right-side decision cluster in diagram 2
     further right so the center "append step" box doesn't collide with the
     "decide next node" diamond.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

EXCAL_DIR = Path(__file__).parent / "excalidraw"


def _bump_cramped_text(elements: list[dict]) -> int:
    """Bump font size on cramped text, and widen its parent rectangle.

    A "cramped" text is 11pt or 12pt with width < 200 (i.e., not enough room
    to read). Bump fontSize to 14 and widen the parent rectangle by the same
    amount of extra width needed.
    """
    # Build a lookup: (x, y, w, h) -> rectangle element
    rects_by_box: dict[tuple, dict] = {}
    for el in elements:
        if el.get("type") == "rectangle":
            key = (
                round(el.get("x", 0), 1),
                round(el.get("y", 0), 1),
                round(el.get("width", 0), 1),
                round(el.get("height", 0), 1),
            )
            rects_by_box[key] = el

    changes = 0
    for el in elements:
        if el.get("type") != "text":
            continue
        if el.get("isDeleted"):
            continue
        fs = el.get("fontSize", 16)
        w = el.get("width", 0)
        h = el.get("height", 0)
        if fs >= 14:
            continue
        # Bump font size
        new_fs = 14 if fs <= 12 else fs + 2
        old_fs = el["fontSize"]
        el["fontSize"] = new_fs
        # If the box is narrow, widen it. Bumping 11→14 means ~27% taller,
        # and we want at least 1.4× the text height to fit comfortably.
        if w < 200 or h < 30:
            new_w = max(w, 200) if w < 200 else w
            new_h = max(h, 36) if h < 30 else h
            old_key = (
                round(el.get("x", 0), 1),
                round(el.get("y", 0), 1),
                round(w, 1),
                round(h, 1),
            )
            el["width"] = new_w
            el["height"] = new_h
            # If a parent rectangle matches the old box, resize it too
            rect = rects_by_box.get(old_key)
            if rect is not None:
                rect["width"] = new_w
                rect["height"] = new_h
        changes += 1
        print(
            f"  text: {old_fs}pt {int(w)}x{int(h)} -> {new_fs}pt "
            f"{int(el['width'])}x{int(el['height'])}  "
            f"text={(el.get('text') or '')[:30]!r}",
            file=sys.stderr,
        )
    return changes


def _shift_right_cluster(elements: list[dict], match_text: str, dx: int) -> int:
    """Shift every element whose text contains `match_text` by (dx, 0)."""
    moved = 0
    for el in elements:
        t = (el.get("text") or "")
        if match_text.lower() in t.lower():
            el["x"] = el.get("x", 0) + dx
            moved += 1
    return moved


def _shift_xrange(elements: list[dict], x_min: float, dx: int) -> int:
    """Shift every element whose x >= x_min by (dx, 0). Used to move a
    geometric cluster that has no common text marker."""
    moved = 0
    for el in elements:
        if el.get("x", 0) >= x_min:
            el["x"] = el.get("x", 0) + dx
            moved += 1
    return moved


def process(file: Path) -> None:
    print(f"== {file.name} ==", file=sys.stderr)
    data = json.loads(file.read_text())
    elements = data.get("elements", [])
    n = _bump_cramped_text(elements)
    if "02-pipeline-execution" in file.name:
        # The "decide next node" diamond is in the right half. Look at the
        # x position of the diamond text to find the cut-over, then shift
        # everything rightward.
        diamond = next(
            (
                el for el in elements
                if (el.get("text") or "").lower().startswith("decide next node")
            ),
            None,
        )
        if diamond:
            x_min = diamond["x"] - 80
            moved = _shift_xrange(elements, x_min, 160)
            print(f"  shift right cluster (x>={int(x_min)}) by +160: {moved} elements moved", file=sys.stderr)
    # Recompute appState — Excalidraw uses the bounding box of elements for
    # view bounds. Set a generous grid size to keep things tidy.
    data["elements"] = elements
    file.write_text(json.dumps(data, indent=2))
    print(f"  wrote {file} ({n} text elements resized)", file=sys.stderr)


def main() -> None:
    for p in sorted(EXCAL_DIR.glob("*.excalidraw")):
        process(p)


if __name__ == "__main__":
    main()
