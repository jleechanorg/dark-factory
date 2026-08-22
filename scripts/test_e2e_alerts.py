#!/usr/bin/env python3
"""E2E Alerting Verification Test for Dark Factory.

Tests live dispatch of:
1. Slack Bot alerts to #auto-factory (channel C0AH3RY3DK6) via HERMES_SLACK_BOT_TOKEN
2. Gmail SMTP alert delivery to jleechan@gmail.com via EMAIL_USER / EMAIL_PASS
"""

import os
import sys
import json
import ssl
import smtplib
import urllib.request
import datetime

def test_slack_alert():
    token = os.environ.get("HERMES_SLACK_BOT_TOKEN") or os.environ.get("OPENCLAW_SLACK_BOT_TOKEN")
    channel = os.environ.get("DARK_FACTORY_SLACK_CHANNEL", "C0AH3RY3DK6")
    if not token:
        print("❌ [Slack Alert] HERMES_SLACK_BOT_TOKEN / OPENCLAW_SLACK_BOT_TOKEN not found", file=sys.stderr)
        return False

    now = datetime.datetime.now(datetime.timezone.utc).isoformat()
    msg = f"🧪 *[Dark Factory Automated E2E Alert Test]*\nTimestamp: `{now}`\nHost: `jeff-ubuntu`\nVerdict: `VERIFIED`"
    payload = json.dumps({
        "channel": channel,
        "text": msg
    }).encode("utf-8")

    req = urllib.request.Request(
        "https://slack.com/api/chat.postMessage",
        data=payload,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json; charset=utf-8"
        }
    )

    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read().decode("utf-8"))
            if data.get("ok"):
                print(f"✅ [Slack Alert] Delivered to channel {channel} (ts={data.get('ts')})")
                return True
            else:
                print(f"❌ [Slack Alert] API error: {data.get('error')}", file=sys.stderr)
                return False
    except Exception as e:
        print(f"❌ [Slack Alert] HTTP exception: {e}", file=sys.stderr)
        return False

def test_email_alert():
    user = os.environ.get("EMAIL_USER")
    pwd = os.environ.get("EMAIL_PASS")
    recipient = os.environ.get("BACKUP_EMAIL", user)

    if not user or not pwd:
        print("❌ [Email Alert] EMAIL_USER or EMAIL_PASS not set", file=sys.stderr)
        return False

    now = datetime.datetime.now(datetime.timezone.utc).isoformat()
    from email.message import EmailMessage
    msg = EmailMessage()
    msg.set_content(f"Dark Factory Automated E2E Alert Test\n\nTimestamp: {now}\nHost: jeff-ubuntu\nStatus: VERIFIED\n")
    msg["Subject"] = f"[Dark Factory E2E Test] Alert Delivery Probe ({now})"
    msg["From"] = user
    msg["To"] = recipient

    try:
        context = ssl.create_default_context()
        with smtplib.SMTP_SSL("smtp.gmail.com", 465, context=context, timeout=10) as server:
            server.login(user, pwd)
            server.send_message(msg)
            print(f"✅ [Email Alert] Delivered via Gmail SMTP to {recipient}")
            return True
    except Exception as e:
        print(f"❌ [Email Alert] SMTP exception: {e}", file=sys.stderr)
        return False

def main():
    print("===============================================================================")
    print(" Running Dark Factory E2E Alert Delivery Tests")
    print("===============================================================================")
    slack_ok = test_slack_alert()
    email_ok = test_email_alert()

    if slack_ok and email_ok:
        print("===============================================================================")
        print(" 🎉 ALL E2E ALERT DELIVERY TESTS PASSED")
        print("===============================================================================")
        sys.exit(0)
    else:
        print("===============================================================================")
        print(" ❌ E2E ALERT TESTS FAILED")
        print("===============================================================================")
        sys.exit(1)

if __name__ == "__main__":
    main()
