#!/usr/bin/env python3
# github-webhook-listener.py — minimal localhost:9876 HTTP server that
# validates HMAC-SHA256 GitHub webhooks and, on a push event whose repo
# matches the GH_REPOS allow-list, invokes post-webhook-beacon.sh to post
# a milestone beacon to #factory (C0BGEC77EP4).
#
# Why Python: the dark-factory daemon already requires python3 for the
# br show --json fallback and the libnotify-slack.sh JSON-escape helper.
# Avoiding a second runtime (Node, Go, etc.) keeps the operator's setup
# trivial. stdlib only — no pip dependencies.
#
# Usage (operator):
#   github-webhook-listener.py                # listens on 127.0.0.1:9876
#   PORT=9877 github-webhook-listener.py      # override port
#   LOG_LEVEL=debug github-webhook-listener.py
#
# Invocation:
#   The launchd template ai.dark-factory.github-webhook.plist.template
#   launches this script via launchd-wrapper.sh (PATH-bridge) so the
#   underlying beacon script can resolve its siblings in daemon/scripts/.
#
# Endpoints:
#   POST /webhook        GitHub webhook receiver (HMAC-validated)
#   GET  /healthz        returns "OK" (200); used by launchd print probes
#
# Env (set by the launchd plist EnvironmentVariables dict):
#   GITHUB_WEBHOOK_SECRET   shared secret for X-Hub-Signature-256 validation
#                           (REQUIRED; refuse to start if empty)
#   FACTORY_SLACK_CHANNEL_ID  defaults to C0BGEC77EP4 (the public #factory id)
#   HERMES_SLACK_BOT_TOKEN   forwarded to post-webhook-beacon.sh
#   GH_REPOS                  comma-separated list of full_name repos to
#                             accept (e.g. 'jleechanorg/worldarchitect.ai,
#                             jleechanorg/dark-factory'); empty = accept all
#   PORT                      default 9876
#   BEACON_SCRIPT             default $REPO_ROOT/daemon/scripts/post-webhook-beacon.sh
#   REPO_ROOT                 default /Users/jleechan/projects/dark-factory
import hashlib
import hmac
import json
import logging
import os
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG_LEVEL = os.environ.get("LOG_LEVEL", "info").upper()
logging.basicConfig(
    level=LOG_LEVEL,
    format="%(asctime)s [%(levelname)s] %(message)s",
    stream=sys.stdout,
)
log = logging.getLogger("gh-webhook")

SECRET = os.environ.get("GITHUB_WEBHOOK_SECRET", "").encode("utf-8")
PORT = int(os.environ.get("PORT", "9876"))
GH_REPOS_RAW = os.environ.get("GH_REPOS", "").strip()
GH_REPOS = {r.strip() for r in GH_REPOS_RAW.split(",") if r.strip()}
FACTORY_SLACK_CHANNEL_ID = os.environ.get("FACTORY_SLACK_CHANNEL_ID", "C0BGEC77EP4")
HERMES_SLACK_BOT_TOKEN = os.environ.get("HERMES_SLACK_BOT_TOKEN", "")
REPO_ROOT = os.environ.get(
    "REPO_ROOT", os.path.expanduser("~/projects/dark-factory")
)
BEACON_SCRIPT = os.environ.get(
    "BEACON_SCRIPT",
    os.path.join(REPO_ROOT, "daemon/scripts/post-webhook-beacon.sh"),
)

if not SECRET:
    log.error("GITHUB_WEBHOOK_SECRET is empty; refusing to start. "
              "Set it in the launchd plist EnvironmentVariables.")
    sys.exit(2)

if not os.path.isfile(BEACON_SCRIPT):
    log.warning("BEACON_SCRIPT not found at %s; pushes will be accepted but "
                "no beacon will fire (verify post-webhook-beacon.sh is "
                "checked out at the right path).", BEACON_SCRIPT)


def verify_signature(secret: bytes, body: bytes, signature_header: str) -> bool:
    """Validate GitHub's X-Hub-Signature-256 header (sha256=<hex>)."""
    if not signature_header or not signature_header.startswith("sha256="):
        return False
    provided = signature_header.split("=", 1)[1].strip()
    mac = hmac.new(secret, msg=body, digestmod=hashlib.sha256)
    expected = mac.hexdigest()
    # Constant-time comparison to avoid timing oracles.
    return hmac.compare_digest(provided, expected)


def repo_allowed(full_name: str) -> bool:
    """If GH_REPOS is empty, accept all repos (operator opt-in)."""
    if not GH_REPOS:
        return True
    return full_name in GH_REPOS


def short_branch(ref: str) -> str:
    """Convert refs/heads/main -> main; refs/tags/v1.2 -> tags/v1.2."""
    if ref.startswith("refs/heads/"):
        return ref[len("refs/heads/"):]
    return ref


def truncate_message(msg: str, n: int = 80) -> str:
    """First line of a commit message, truncated."""
    if not msg:
        return ""
    first = msg.split("\n", 1)[0]
    if len(first) > n:
        first = first[: n - 3] + "..."
    return first


def fire_beacon(env_overrides: dict) -> int:
    """Invoke post-webhook-beacon.sh with the payload-derived env."""
    if not os.path.isfile(BEACON_SCRIPT):
        log.warning("Skipping beacon; BEACON_SCRIPT missing: %s", BEACON_SCRIPT)
        return 1
    env = os.environ.copy()
    env.update(env_overrides)
    try:
        result = subprocess.run(
            [BEACON_SCRIPT],
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
        if result.returncode != 0:
            log.warning("beacon exit %s: stdout=%r stderr=%r",
                        result.returncode, result.stdout, result.stderr)
        return result.returncode
    except subprocess.TimeoutExpired:
        log.warning("beacon timed out after 10s")
        return 124
    except Exception as e:  # pragma: no cover — defensive
        log.warning("beacon invocation failed: %s", e)
        return 1


class Handler(BaseHTTPRequestHandler):
    """Minimal HTTP handler. Stdlib only."""

    # Silence default per-request stderr logging; route through our log.
    def log_message(self, fmt, *args):
        log.debug("%s - %s", self.address_string(), fmt % args)

    # ----- /healthz ---------------------------------------------------------
    def do_GET(self):
        if self.path.startswith("/healthz"):
            body = b"OK\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_error(404, "not found")

    # ----- /webhook ---------------------------------------------------------
    def do_POST(self):
        if not self.path.startswith("/webhook"):
            self.send_error(404, "not found")
            return

        # Read full body (GitHub sends a single POST; Content-Length is set).
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = 0
        if length <= 0 or length > 5 * 1024 * 1024:  # 5 MiB hard cap
            log.warning("rejecting POST: bad Content-Length=%s", length)
            self.send_error(411, "Content-Length required (and <=5MiB)")
            return
        body = self.rfile.read(length)

        # Verify HMAC before parsing JSON (cheaper fail path).
        sig_header = self.headers.get("X-Hub-Signature-256", "")
        if not verify_signature(SECRET, body, sig_header):
            log.warning("rejecting POST: bad signature (path=%s, ua=%s)",
                        self.path, self.headers.get("User-Agent", ""))
            self.send_error(401, "signature verification failed")
            return

        # Parse payload.
        try:
            payload = json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as e:
            log.warning("rejecting POST: bad JSON: %s", e)
            self.send_error(400, "invalid JSON")
            return

        # We only care about push events.
        event = self.headers.get("X-GitHub-Event", "")
        if event != "push":
            log.info("ignoring non-push event: %s", event)
            self._respond(200, {"ignored": event})
            return

        repo = (payload.get("repository") or {}).get("full_name", "")
        if not repo:
            log.warning("push event missing repository.full_name")
            self._respond(200, {"ignored": "missing repo"})
            return

        if not repo_allowed(repo):
            log.info("ignoring push from non-allow-listed repo: %s "
                     "(GH_REPOS=%s)", repo, sorted(GH_REPOS))
            self._respond(200, {"ignored": repo})
            return

        ref = payload.get("ref", "")
        branch = short_branch(ref)
        pusher = (payload.get("pusher") or {}).get("name", "unknown")
        head_commit = payload.get("head_commit") or {}
        head_sha = head_commit.get("id", "")
        head_message = head_commit.get("message", "")
        compare = payload.get("compare", "")

        log.info("push accepted: repo=%s ref=%s sha=%s pusher=%s",
                 repo, ref, head_sha[:8], pusher)

        # Fire the beacon (synchronous; beacon itself is a fire-and-forget
        # slack_post which is async by default).
        rc = fire_beacon({
            "WEBHOOK_REPO": repo,
            "WEBHOOK_REF": ref,
            "WEBHOOK_BRANCH": branch,
            "WEBHOOK_PUSHER": pusher,
            "WEBHOOK_HEAD_SHA": head_sha,
            "WEBHOOK_HEAD_MESSAGE": head_message,
            "WEBHOOK_COMPARE": compare,
            "FACTORY_SLACK_CHANNEL_ID": FACTORY_SLACK_CHANNEL_ID,
            "HERMES_SLACK_BOT_TOKEN": HERMES_SLACK_BOT_TOKEN,
        })
        self._respond(200, {"accepted": repo, "branch": branch,
                            "beacon_rc": rc})

    # ---- helpers ----
    def _respond(self, code: int, body: dict):
        data = json.dumps(body).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main() -> int:
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    log.info("github-webhook-listener listening on 127.0.0.1:%d "
             "(repos=%s)", PORT, sorted(GH_REPOS) if GH_REPOS else ["<all>"])
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log.info("interrupted; shutting down")
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())