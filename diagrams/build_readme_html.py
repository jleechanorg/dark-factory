#!/usr/bin/env python3
"""Build README.html from README.md.

This script:
  1. Reads `README.md` at the repo root.
  2. Replaces the four ASCII / Mermaid diagram blocks with semantic-color
     Excalidraw PNGs that follow the dark-factory color contract.
  3. Stitches the markdown body into a styled HTML document with the
     design tokens, sticky TOC, and code-block highlighting.

Usage:
    python diagrams/build_readme_html.py            # writes README.html
    python diagrams/build_readme_html.py --output   # custom path
"""
from __future__ import annotations

import argparse
import html
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
README_MD = REPO / "README.md"
README_HTML = REPO / "README.html"

# ---------------------------------------------------------------------------
# Diagram substitutions
# Each entry: (opening fence + content + closing fence, replacement HTML)
# ---------------------------------------------------------------------------

_DIAGRAMS = {
    # Simple 3-layer ASCII intro (lines ~230-247)
    "ascii_3layer": {
        "marker": "```\n┌─────────────────────────────────────────────────────────────────────────┐\n│ Layer 3: Pipeline Engine (Dark Factory)",
        "img": '<img class="diagram" src="diagrams/png/01-3-layer-convergence.png" '
               'alt="3-layer convergence + operator isolation" loading="lazy" />',
    },
    # Detailed Mermaid system architecture (lines ~255-285)
    "mermaid_system": {
        "marker": "```mermaid\nflowchart LR\n  subgraph OP[\"Operator / human",
        "img": '<img class="diagram diagram-wide" '
               'src="diagrams/png/01-3-layer-convergence.png" '
               'alt="3-layer convergence + operator isolation + CXDB/Healer" '
               'loading="lazy" />',
    },
    # Mermaid pipeline execution flow (lines ~298-313)
    "mermaid_pipeline": {
        "marker": "```mermaid\nflowchart LR\n  S([\"start\"]) --> R{\"resolve(node)\"}",
        "img": '<img class="diagram" src="diagrams/png/02-pipeline-execution.png" '
               'alt="Pipeline execution — one step of the walk" loading="lazy" />',
    },
    # ASCII CXDB/Healer loop (lines ~376-386)
    "ascii_cxdb_healer": {
        "marker": "```\n ┌────────────────┐       Nodes Execute",
        "img": '<img class="diagram" src="diagrams/png/03-cxdb-healer-loop.png" '
               'alt="CXDB + Healer feedback loop" loading="lazy" />',
    },
}


def _read_md() -> str:
    return README_MD.read_text()


def _apply_diagram_substitutions(md: str) -> str:
    out = md
    for key, spec in _DIAGRAMS.items():
        # Find the code block starting at marker. Code blocks end at the next
        # line that is exactly "```".
        start = out.find(spec["marker"])
        if start < 0:
            print(f"[warn] diagram marker not found: {key}", file=sys.stderr)
            continue
        # Find the closing fence after `start`
        end = out.find("\n```\n", start)
        if end < 0:
            print(f"[warn] closing fence not found for {key}", file=sys.stderr)
            continue
        end += len("\n```\n")
        out = out[:start] + spec["img"] + "\n" + out[end:]
        print(f"[ok] replaced {key}", file=sys.stderr)
    return out


# ---------------------------------------------------------------------------
# Markdown → HTML (a small subset that fits the README structure)
# ---------------------------------------------------------------------------

def _md_to_html(md: str) -> str:
    """Convert markdown to HTML for the specific subset used in README.md.

    Handles: headings (h1-h3), paragraphs, fenced code, inline code, **bold**,
    *italic*, [text](url) links, and bullet lists. Anything else passes
    through as text.
    """
    lines = md.split("\n")
    out: list[str] = []
    in_code = False
    in_list = False
    list_items: list[str] = []
    para: list[str] = []

    def flush_para() -> None:
        if para:
            out.append(f"<p>{_inline(' '.join(para))}</p>")
            para.clear()

    def flush_list() -> None:
        nonlocal in_list
        if list_items:
            out.append("<ul>" + "".join(list_items) + "</ul>")
            list_items.clear()
        in_list = False

    def flush() -> None:
        flush_para()
        flush_list()

    for raw in lines:
        line = raw.rstrip()

        if line.startswith("```"):
            flush()
            if not in_code:
                in_code = True
                lang = line[3:].strip()
                out.append(f'<pre class="code" data-lang="{html.escape(lang)}"><code>')
            else:
                in_code = False
                out.append("</code></pre>")
            continue
        if in_code:
            out.append(html.escape(raw))
            continue

        # Headings
        m = re.match(r"^(#{1,4})\s+(.*)$", line)
        if m:
            flush()
            level = len(m.group(1))
            text = _inline(m.group(2))
            out.append(f"<h{level} id=\"{_slug(text)}\">{text}</h{level}>")
            continue

        # Horizontal rule
        if line.strip() == "---":
            flush()
            out.append("<hr />")
            continue

        # Block image (the diagram substitutions)
        if line.startswith("<img "):
            flush()
            out.append(f'<figure class="diagram-figure">{line}</figure>')
            continue

        # Blank line → flush
        if not line.strip():
            flush()
            continue

        # List item
        m = re.match(r"^(\s*)[-*]\s+(.*)$", line)
        if m:
            flush_para()
            in_list = True
            list_items.append(f"<li>{_inline(m.group(2))}</li>")
            continue

        # Otherwise accumulate as paragraph text
        if in_list:
            # Continuation of a list item? For the README, lists are single-line.
            flush()
        para.append(line)

    flush()
    return "\n".join(out)


def _inline(text: str) -> str:
    """Apply inline transformations: bold, italic, inline code, links, and
    any HTML that's already embedded (preserves our <img> substitutions)."""
    # First escape everything that isn't already an HTML tag we trust.
    # We allow <img> through; everything else is escaped then re-allowed
    # via token replacement.
    img_re = re.compile(r'<img [^>]+>')
    img_tokens: list[str] = []

    def stash_img(m: re.Match) -> str:
        img_tokens.append(m.group(0))
        return f"\x00IMG{len(img_tokens) - 1}\x00"

    text = img_re.sub(stash_img, text)
    text = html.escape(text, quote=False)

    # Inline code
    text = re.sub(
        r"`([^`]+)`",
        lambda m: f"<code>{m.group(1)}</code>",
        text,
    )
    # Bold
    text = re.sub(
        r"\*\*([^*]+)\*\*",
        lambda m: f"<strong>{m.group(1)}</strong>",
        text,
    )
    # Italic
    text = re.sub(
        r"(?<![*])\*([^*\n]+)\*(?![*])",
        lambda m: f"<em>{m.group(1)}</em>",
        text,
    )
    # Links: [text](url)
    text = re.sub(
        r"\[([^\]]+)\]\(([^)]+)\)",
        lambda m: f'<a href="{m.group(2)}">{m.group(1)}</a>',
        text,
    )
    # Restore image tokens
    text = re.sub(
        r"\x00IMG(\d+)\x00",
        lambda m: img_tokens[int(m.group(1))],
        text,
    )
    return text


def _slug(text: str) -> str:
    s = re.sub(r"<[^>]+>", "", text)
    s = re.sub(r"[^\w\s-]", "", s).strip().lower()
    s = re.sub(r"[\s_-]+", "-", s)
    return s


# ---------------------------------------------------------------------------
# HTML template
# ---------------------------------------------------------------------------

_CSS = """
:root {
  --bg: #f2efe7;
  --panel: #fffdf8;
  --ink: #1f1a16;
  --muted: #6f6357;
  --brand: #165a72;
  --brand-soft: #d8e7ec;
  --line: #dfd3c2;
  --code-bg: #1a1714;
  --code-fg: #e7d9c4;
  --pass: #15803d;
  --warn: #a16207;
  --fail: #b91c1c;
  --error: #6d28d9;
  --max-w: 920px;
}
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text",
               "Helvetica Neue", Helvetica, Arial, sans-serif;
  font-size: 16.5px;
  line-height: 1.62;
  color: var(--ink);
  background: var(--bg);
}
.layout {
  display: grid;
  grid-template-columns: 240px 1fr;
  gap: 48px;
  max-width: 1280px;
  margin: 0 auto;
  padding: 48px 32px 96px;
}
.toc {
  position: sticky;
  top: 32px;
  align-self: start;
  font-size: 14px;
  line-height: 1.5;
  border-left: 2px solid var(--line);
  padding: 12px 0 12px 20px;
  max-height: calc(100vh - 64px);
  overflow: auto;
}
.toc h4 {
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--muted);
  margin: 0 0 10px;
  font-weight: 600;
}
.toc ol { list-style: decimal; padding-left: 18px; margin: 0; }
.toc ol ol { padding-left: 16px; margin-top: 4px; }
.toc li { margin: 3px 0; }
.toc a {
  color: var(--ink);
  text-decoration: none;
  border-bottom: 1px dashed transparent;
  transition: border-color 0.12s, color 0.12s;
}
.toc a:hover {
  color: var(--brand);
  border-bottom-color: var(--brand);
}
main { max-width: var(--max-w); }
h1, h2, h3, h4 { color: var(--ink); line-height: 1.25; }
h1 {
  font-size: 38px;
  margin: 0 0 8px;
  letter-spacing: -0.01em;
}
h1 + p { color: var(--muted); margin-top: 0; }
h2 {
  font-size: 26px;
  margin: 56px 0 14px;
  padding-top: 16px;
  border-top: 1px solid var(--line);
}
h2:first-of-type { border-top: 0; padding-top: 0; }
h3 { font-size: 20px; margin: 32px 0 10px; }
h4 { font-size: 16px; margin: 20px 0 6px; }
p { margin: 12px 0; }
hr { border: 0; border-top: 1px solid var(--line); margin: 36px 0; }
ul { padding-left: 22px; }
li { margin: 4px 0; }
a { color: var(--brand); }
a:hover { text-decoration: underline; }
code {
  font-family: "SF Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace;
  font-size: 0.92em;
  background: rgba(22, 90, 114, 0.08);
  color: var(--brand);
  padding: 1px 5px;
  border-radius: 3px;
}
pre.code {
  background: var(--code-bg);
  color: var(--code-fg);
  padding: 18px 20px;
  border-radius: 8px;
  overflow-x: auto;
  font-size: 13.5px;
  line-height: 1.55;
  margin: 18px 0;
  border: 1px solid #2a2520;
}
pre.code code { background: transparent; color: inherit; padding: 0; }
.diagram-figure {
  margin: 24px -12px;
  text-align: center;
}
.diagram-figure .diagram {
  max-width: 100%;
  height: auto;
  border-radius: 8px;
  background: var(--panel);
  border: 1px solid var(--line);
  box-shadow: 0 1px 2px rgba(31, 26, 22, 0.04);
}
.diagram-figure .diagram-wide { max-width: 110%; }
table { border-collapse: collapse; width: 100%; margin: 16px 0; font-size: 14.5px; }
th, td { padding: 8px 12px; text-align: left; border-bottom: 1px solid var(--line); }
th { color: var(--muted); font-weight: 600; font-size: 12px; text-transform: uppercase; letter-spacing: 0.05em; }
tr:hover td { background: var(--brand-soft); }
blockquote {
  border-left: 3px solid var(--brand);
  margin: 18px 0;
  padding: 4px 16px;
  color: var(--muted);
  background: var(--panel);
  border-radius: 0 4px 4px 0;
}
@media (max-width: 980px) {
  .layout { grid-template-columns: 1fr; }
  .toc { position: static; max-height: none; border-left: 0; padding-left: 0; }
  .diagram-figure { margin: 24px 0; }
}
img.emoji, h1 img, h2 img { height: 1em; vertical-align: -0.15em; }
"""


def _build_toc(md: str) -> str:
    """Pull the existing markdown TOC (lines 62-76) and convert it to a
    side-bar nav with the same items, stripped of emojis and the `1. ` prefix."""
    lines = md.split("\n")
    toc_lines: list[str] = []
    in_toc = False
    for line in lines:
        if line.strip() == "## 📑 Table of Contents":
            in_toc = True
            continue
        if in_toc:
            if line.strip() == "---":
                break
            if line.strip():
                toc_lines.append(line.rstrip())
    # Build nested ol from indentation
    items: list[tuple[int, str]] = []
    for ln in toc_lines:
        m = re.match(r"^(\s*)\d+\.\s+\[(.+?)\]\(#(.+?)\)\s*$", ln)
        if not m:
            continue
        depth = len(m.group(1)) // 2
        title = m.group(2)
        # Strip leading emoji + space for cleaner sidebar look
        title = re.sub(r"^[^A-Za-z0-9#]+", "", title)
        items.append((depth, title))
    if not items:
        return ""
    # Build nested ordered list
    html_parts: list[str] = ["<aside class=\"toc\"><h4>On this page</h4><ol>"]
    last_depth = 0
    open_lists: list[bool] = [True]  # depth 0 always open
    for depth, title in items:
        while depth > len(open_lists) - 1:
            html_parts.append("<ol>")
            open_lists.append(True)
        while depth < len(open_lists) - 1:
            html_parts.append("</ol></li>")
            open_lists.pop()
        if depth == len(open_lists) - 1 and depth > 0:
            html_parts.append("</li>")
        html_parts.append(f'<li><a href="#{_slug(title)}">{html.escape(title)}</a>')
    while len(open_lists) > 1:
        html_parts.append("</li></ol>")
        open_lists.pop()
    html_parts.append("</li></ol></aside>")
    return "\n".join(html_parts)


def build() -> str:
    md = _read_md()
    md = _apply_diagram_substitutions(md)
    body = _md_to_html(md)
    toc = _build_toc(md)

    page = f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Dark Factory — Attractor-Pattern DOT Pipeline Runner</title>
  <style>{_CSS}</style>
</head>
<body>
  <div class="layout">
    {toc}
    <main>
      {body}
    </main>
  </div>
</body>
</html>
"""
    return page


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--output", type=Path, default=README_HTML)
    args = p.parse_args()
    html = build()
    args.output.write_text(html)
    print(f"wrote {args.output}  ({len(html):,} bytes)")


if __name__ == "__main__":
    main()
