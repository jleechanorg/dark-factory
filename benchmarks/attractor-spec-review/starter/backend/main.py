def app() -> str:
    """Minimal backend health endpoint for full-pipeline smoke checks."""
    return "attractor-spec-review backend ready"


if __name__ == "__main__":
    print(app())
