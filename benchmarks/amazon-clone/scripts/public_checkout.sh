#!/usr/bin/env bash
# Public checkout/orders gate for the Amazon benchmark candidate.

set -euo pipefail

WORKDIR="${1:-.}"
cd "$WORKDIR"

PORT="${PORT:-31345}"
BASE_URL="http://127.0.0.1:${PORT}"

fail() {
    echo "CHECKOUT_FAIL: $*" >&2
    exit 1
}

for file in Makefile package.json firebase.json firestore.rules src/server.js src/public/index.html src/public/app.js src/public/styles.css; do
    [ -f "$file" ] || fail "missing required file: $file"
done

echo "== cart regression gate =="
bash benchmarks/amazon-clone/scripts/public_cart.sh .

echo "== checkout static checks =="
python - <<'PY'
from pathlib import Path

source = "\n".join(
    p.read_text(errors="ignore")
    for p in Path(".").glob("**/*")
    if p.is_file()
    and "node_modules" not in p.parts
    and "package-lock.json" not in p.parts
    and "results" not in p.parts
    and p.suffix in {".js", ".json", ".html", ".css", ".rules", ".md"}
)
required = [
    "POST /orders",
    "GET /orders",
    "/orders/:id",
    "/orders/:id/reorder",
    "reorder",
    "POST /checkout",
    "POST /checkout/summary",
    "maskedCard",
    "subtotalCents",
    "grandTotalCents",
    "ordersCreated",
    "checkoutAttempts",
]
missing = [term for term in required if term.lower() not in source.lower()]
if missing:
    raise SystemExit(f"missing checkout terms: {', '.join(missing)}")
print("checkout static checks passed")
PY

echo "== checkout runtime smoke =="
PORT="$PORT" make run > /tmp/amazon-checkout-run.log 2>&1 &
server_pid=$!
cleanup() {
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
}
trap cleanup EXIT

python - "$BASE_URL" <<'PY'
import http.cookiejar
import json
import re
import sys
import time
import urllib.error
import urllib.request

base = sys.argv[1]
jar = http.cookiejar.CookieJar()
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))


def fetch(path, method="GET", payload=None, expected=(200,), headers=None):
    data = None
    req_headers = {"Accept": "application/json"}
    if headers:
        req_headers.update(headers)
    if payload is not None:
        data = json.dumps(payload).encode()
        req_headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, headers=req_headers, method=method)
    try:
        with opener.open(req, timeout=5) as resp:
            body = resp.read().decode()
            if resp.status not in expected:
                raise RuntimeError(f"{path} status {resp.status}, expected {expected}")
            return resp.status, body
    except urllib.error.HTTPError as exc:
        body = exc.read().decode()
        if exc.code in expected:
            return exc.code, body
        raise


def parse_json(body):
    return json.loads(body)


for _ in range(60):
    try:
        _, health = fetch("/health")
        break
    except Exception:
        time.sleep(0.25)
else:
    raise SystemExit("server did not become healthy")

fetch("/seed/reset", method="POST", expected=(200, 201, 204))

_, login = fetch("/auth/login", method="POST", payload={"username": "shopper", "password": "password"}, expected=(200, 201))
login_payload = parse_json(login)
if isinstance(login_payload, dict) and login_payload.get("user") is not None:
    pass

products = parse_json(fetch("/products")[1])
if isinstance(products, dict):
    products = products.get("products") or products.get("items") or products.get("data") or []
if not isinstance(products, list) or not products:
    raise SystemExit("no products available for checkout smoke")
product_id = products[0].get("id") or products[0].get("productId") or products[0].get("sku")
if not product_id:
    raise SystemExit("product missing id for checkout smoke")

_, cart = fetch("/api/cart/items", method="POST", payload={"productId": product_id, "quantity": 1}, expected=(200, 201))
cart_body = parse_json(cart)
if not cart_body:
    raise SystemExit("cart add returned empty body")

address_payload = {
    "recipient": "Checkout Shopper",
    "street": "101 Check Ave",
    "unit": "5B",
    "city": "Seattle",
    "region": "WA",
    "postalCode": "98101",
    "country": "US",
    "phone": "2065551212",
}
_, address_body = fetch("/api/addresses", method="POST", payload=address_payload, expected=(200, 201))
address = parse_json(address_body)
if isinstance(address, dict) and "address" in address:
    address = address["address"]
address_id = address.get("id") if isinstance(address, dict) else None
if not address_id:
    raise SystemExit("address creation did not return id")

_, _ = fetch(f"/api/addresses/{address_id}/default", method="POST", expected=(200, 204))

checkout_payloads = [
    {
        "shippingAddress": {
            "name": "Checkout Shopper",
            "line1": "101 Check Ave",
            "line2": "",
            "city": "Seattle",
            "state": "WA",
            "postalCode": "98101",
            "country": "US",
        },
        "payment": {
            "cardNumber": "4111111111111111",
            "expiry": "12/30",
            "cvv": "123",
        },
    },
    {
        "email": "shopper@example.com",
        "fullName": "Checkout Shopper",
        "address": "101 Check Ave, Seattle, WA 98101",
        "city": "Seattle",
        "state": "WA",
        "zip": "98101",
        "cardNumber": "4111111111111111",
        "expiryDate": "12/30",
        "cvv": "123",
    },
]

checkout_endpoints = [
    "/api/orders",
    "/api/checkout",
    "/orders",
    "/checkout",
]

order = None
for endpoint in checkout_endpoints:
    for payload in checkout_payloads:
        try:
            _, body = fetch(endpoint, method="POST", payload=payload, expected=(200, 201, 204))
            order = parse_json(body) if isinstance(body, str) else {}
            if isinstance(order, dict) and "id" in order:
                break
            if isinstance(order, dict) and order.get("orderId"):
                order["id"] = order["orderId"]
                break
        except Exception:
            continue
    if isinstance(order, dict) and "id" in order:
        break

if not isinstance(order, dict) or "id" not in order:
    raise SystemExit("checkout request did not return a created order")

if "maskedCard" in order:
    if "4111111111111111" in str(order["maskedCard"]):
        raise SystemExit("order response contains full card number")
elif "cardNumber" in order and "411111" not in str(order["cardNumber"]):
    # if raw card persisted, fail hard
    raise SystemExit("order response exposed full card-like number")

order_id = order["id"]

_, history_body = fetch("/api/orders", expected=(200, 304, 302))
history = parse_json(history_body)
if not isinstance(history, list) or not any(item.get("id") == order_id for item in history):
    raise SystemExit("new order not present in /api/orders history")

_, order_body = fetch(f"/api/orders/{order_id}", expected=(200, 201, 204))
order_detail = parse_json(order_body)
if isinstance(order_detail, dict) and "id" in order_detail and order_detail["id"] != order_id:
    raise SystemExit("order id mismatch between detail and requested id")

try:
    _, reorder_body = fetch(f"/api/orders/{order_id}/reorder", method="POST", expected=(200, 201, 204))
except Exception:
    _, reorder_body = fetch(f"/orders/{order_id}/reorder", method="POST", expected=(200, 201, 204))
reorder = parse_json(reorder_body) if reorder_body else {}
if isinstance(reorder, dict) and "orderId" in reorder and reorder["orderId"] == order_id:
    pass

cart_after = parse_json(fetch("/api/cart")[1])
if isinstance(cart_after, dict) and cart_after.get("items"):
    raise SystemExit("cart should be empty after checkout")

print(json.dumps({
    "order_id": order_id,
    "order_status": order.get("status"),
    "cart_cleared": not bool((cart_after.get("items") if isinstance(cart_after, dict) else [])),
    "history_count": len(history),
    "reorder_probe": bool(reorder),
}))
PY

echo "checkout public acceptance passed"
