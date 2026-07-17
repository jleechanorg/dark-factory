#!/usr/bin/env bash
# github-webhook-listener.sh — wrapper for github-webhook-listener.py.
#
# The actual HTTP server is implemented in Python 3 (stdlib only) at
# github-webhook-listener.py; this shell wrapper exists for two reasons:
#
#   1. The launchd template ai.dark-factory.github-webhook.plist.template
#      invokes a .sh file (consistent with the rest of the dark-factory
#      daemon scripts) and passes through launchd-wrapper.sh so the
#      operator's PATH (python3 in particular) is sourced before exec.
#
#   2. The script's --selftest mode runs a fake POST against the listener
#      so operators can verify the wiring end-to-end without configuring a
#      real GitHub webhook.
#
# Usage:
#   github-webhook-listener.sh              # run the listener (foreground)
#   github-webhook-listener.sh --selftest    # start listener, send signed
#                                            # test push, print result, exit
#   PORT=9877 github-webhook-listener.sh     # override listen port
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LISTENER_PY="$SCRIPT_DIR/github-webhook-listener.py"

if [ ! -r "$LISTENER_PY" ]; then
    echo "[github-webhook-listener.sh] missing $LISTENER_PY" >&2
    exit 1
fi

case "${1:-}" in
    --selftest)
        # Selftest: launch the listener in the background, then POST a fake
        # push payload with a valid HMAC signature, then kill the listener.
        # Requires python3 in PATH; on most macOS dev boxes it lives at
        # /usr/bin/python3 (or in $HOME's PATH after launchd-wrapper.sh).
        if ! command -v python3 >/dev/null 2>&1; then
            echo "[selftest] python3 not in PATH; aborting" >&2
            exit 3
        fi
        # Use a known secret + port so we can POST to it predictably.
        export GITHUB_WEBHOOK_SECRET="selftest-secret-$(date +%s)"
        export PORT="${PORT:-9876}"
        export GH_REPOS="${GH_REPOS:-jleechanorg/test-repo}"
        export FACTORY_SLACK_CHANNEL_ID="${FACTORY_SLACK_CHANNEL_ID:-C0BGEC77EP4}"
        # We set a fake token so slack_post is "capable" but the dry-run
        # beacon path is still fail-soft (curl to slack will 401, fine).
        export HERMES_SLACK_BOT_TOKEN="${HERMES_SLACK_BOT_TOKEN:-xoxb-selftest}"
        export REPO_ROOT="${REPO_ROOT:-$HOME/projects/dark-factory}"
        export BEACON_SCRIPT="${BEACON_SCRIPT:-$REPO_ROOT/daemon/scripts/post-webhook-beacon.sh}"
        export LOG_LEVEL="${LOG_LEVEL:-info}"

        echo "[selftest] starting listener on 127.0.0.1:$PORT (secret=${GITHUB_WEBHOOK_SECRET:0:8}...)"
        python3 "$LISTENER_PY" >/tmp/github-webhook-listener.selftest.log 2>&1 &
        LISTENER_PID=$!
        # Give it a moment to bind.
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            if curl -sf "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then
                break
            fi
            sleep 0.2
        done

        # Build a minimal GitHub push payload + HMAC signature.
        payload='{"ref":"refs/heads/main","repository":{"full_name":"jleechanorg/test-repo"},"pusher":{"name":"selftest"},"head_commit":{"id":"abcdef1234567890","message":"selftest push"},"compare":"https://github.com/jleechanorg/test-repo/compare/aaa...abcdef"}'
        sig="sha256=$(printf '%s' "$payload" | openssl dgst -sha256 -hmac "$GITHUB_WEBHOOK_SECRET" | awk '{print $NF}')"

        echo "[selftest] POST /webhook with valid signature"
        http_code="$(curl -sS -o /tmp/github-webhook-listener.selftest.body -w '%{http_code}' \
            -X POST "http://127.0.0.1:$PORT/webhook" \
            -H "Content-Type: application/json" \
            -H "X-GitHub-Event: push" \
            -H "X-Hub-Signature-256: $sig" \
            --data "$payload" || echo "000")"
        echo "[selftest] HTTP $http_code"
        echo "[selftest] response body: $(cat /tmp/github-webhook-listener.selftest.body 2>/dev/null || echo '<none>')"

        # Test bad-signature path (should 401).
        http_code_bad="$(curl -sS -o /dev/null -w '%{http_code}' \
            -X POST "http://127.0.0.1:$PORT/webhook" \
            -H "Content-Type: application/json" \
            -H "X-GitHub-Event: push" \
            -H "X-Hub-Signature-256: sha256=deadbeef" \
            --data "$payload" || echo "000")"
        echo "[selftest] bad-sig HTTP $http_code_bad (expect 401)"

        # Cleanup.
        kill "$LISTENER_PID" 2>/dev/null || true
        wait "$LISTENER_PID" 2>/dev/null || true
        echo "[selftest] listener log:"
        cat /tmp/github-webhook-listener.selftest.log 2>/dev/null | tail -20 || true

        # PASS criteria: valid-sig returned 200; bad-sig returned 401.
        if [ "$http_code" = "200" ] && [ "$http_code_bad" = "401" ]; then
            echo "OK selftest (valid=200, badsig=401)"
            exit 0
        else
            echo "[selftest] FAIL (valid=$http_code, badsig=$http_code_bad)" >&2
            exit 4
        fi
        ;;
    -h|--help)
        sed -n '2,30p' "$0"
        exit 0
        ;;
esac

# Default: run the listener in the foreground. launchd will keep it alive.
exec python3 "$LISTENER_PY"