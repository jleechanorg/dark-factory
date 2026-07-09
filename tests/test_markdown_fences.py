import pathlib

def test_no_unclosed_markdown_fences():
    """Scan all markdown files in the repository and ensure there are no unclosed backtick fences."""
    root = pathlib.Path(__file__).resolve().parent.parent
    unclosed_files = []
    
    for p in root.rglob("*.md"):
        # Exclude virtual environments, git metadata, and other non-source folders
        if any(part in p.parts for part in (".venv", "venv", ".gemini", "build", "dist", "node_modules", ".git", ".beads")):
            continue
        
        try:
            content = p.read_text(encoding="utf-8")
        except Exception:
            continue
            
        # Count lines starting with ``` to check for unclosed fences
        fences = [line.strip() for line in content.splitlines() if line.strip().startswith("```")]
        if len(fences) % 2 != 0:
            unclosed_files.append(str(p.relative_to(root)))
            
    assert not unclosed_files, f"Found unclosed markdown fences in files: {unclosed_files}"
