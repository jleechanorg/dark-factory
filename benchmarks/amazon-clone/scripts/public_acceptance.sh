#!/usr/bin/env bash
# Public acceptance gate for the Amazon clone benchmark candidate.

set -euo pipefail

WORKDIR="${1:-.}"
cd "$WORKDIR"

PORT="${PORT:-31337}"
BASE_URL="http://127.0.0.1:${PORT}"

fail() {
    echo "PUBLIC_ACCEPTANCE_FAIL: $*" >&2
    exit 1
}

require_file() {
    [ -f "$1" ] || fail "missing required file: $1"
}

require_make_target() {
    awk -F: -v target="$1" '$1 == target { found=1 } END { exit found ? 0 : 1 }' Makefile \
        || fail "missing Makefile target: $1"
}

require_file Makefile
require_file package.json
require_file firebase.json
require_file firestore.rules
require_file src/server.js
require_file src/public/index.html
require_file src/public/app.js
require_file src/public/styles.css

for target in build seed run test validate-size; do
    require_make_target "$target"
done

echo "== contract static checks =="
python - <<'PY'
from pathlib import Path
import json
import sys

root = Path(".")
package = json.loads(Path("package.json").read_text())
deps = {**package.get("dependencies", {}), **package.get("devDependencies", {})}
dep_names = " ".join(sorted(deps))
if "firebase" not in dep_names and "firestore" not in dep_names:
    raise SystemExit("package.json must include Firebase/Firestore dependencies")

source_text = "\n".join(
    p.read_text(errors="ignore")
    for p in root.glob("**/*")
    if p.is_file()
    and "node_modules" not in p.parts
    and "package-lock.json" not in p.parts
    and "results" not in p.parts
    and p.suffix in {".js", ".mjs", ".cjs", ".html", ".css", ".json", ".rules", ".md"}
)
required_terms = [
    "FIRESTORE_EMULATOR_HOST",
    "diagnostics",
    "wishlist",
    "seller",
    "admin",
    "notifications",
    "checkout",
    "order history",
]
missing = [term for term in required_terms if term.lower() not in source_text.lower()]
if missing:
    raise SystemExit(f"missing required visible feature terms: {', '.join(missing)}")

def count_lines(prefixes):
    total = 0
    for path in root.glob("**/*"):
        if not path.is_file():
            continue
        if "node_modules" in path.parts or "package-lock.json" in path.parts or "results" in path.parts:
            continue
        if path.suffix not in {".js", ".mjs", ".cjs", ".html", ".css", ".rules"}:
            continue
        if prefixes and not any(str(path).startswith(prefix) for prefix in prefixes):
            continue
        total += sum(1 for line in path.read_text(errors="ignore").splitlines() if line.strip())
    return total

total_lines = count_lines([])
frontend_lines = count_lines(["src/public/"])
if total_lines < 5000:
    raise SystemExit(f"source line floor not met: {total_lines} < 5000")
if frontend_lines < 2000:
    raise SystemExit(f"frontend line floor not met: {frontend_lines} < 2000")
print(json.dumps({"source_lines": total_lines, "frontend_lines": frontend_lines}))
PY

echo "== make build =="
make build

echo "== make seed =="
make seed

echo "== make test =="
make test

echo "== make validate-size =="
make validate-size

echo "== runtime smoke =="
# Kill any process already listening on PORT
existing_pid=$(lsof -t -i :"$PORT" 2>/dev/null) || true
if [ -n "$existing_pid" ]; then
    kill -9 $existing_pid >/dev/null 2>&1 || true
fi

PORT="$PORT" make run > /tmp/amazon-public-acceptance-run.log 2>&1 &
server_pid=$!
cleanup() {
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
    local pid
    pid=$(lsof -t -i :"$PORT" 2>/dev/null) || true
    if [ -n "$pid" ]; then
        kill -9 $pid >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

python - "$BASE_URL" <<'PY'
import http.cookiejar
import json
import sys
import time
import urllib.error
import urllib.request

base = sys.argv[1]
jar = http.cookiejar.CookieJar()
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))

def fetch(path, method="GET", payload=None, expected=(200,)):
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with opener.open(req, timeout=3) as resp:
            body = resp.read().decode()
            if resp.status not in expected:
                raise RuntimeError(f"{path} status {resp.status}, expected {expected}")
            return body
    except urllib.error.HTTPError as exc:
        if exc.code in expected:
            return exc.read().decode()
        raise

for _ in range(50):
    try:
        fetch("/health")
        break
    except Exception:
        time.sleep(0.2)
else:
    raise SystemExit("server did not become healthy")

health = fetch("/health")
fetch("/metrics")
for path in ("/seed/reset", "/api/seed/reset"):
    fetch(path, method="POST", expected=(200, 201, 204))

products = fetch("/products")
api_products = fetch("/api/products")
diagnostics = None
for path in ("/diagnostics", "/api/diagnostics"):
    try:
        diagnostics = fetch(path)
        break
    except Exception:
        continue
if diagnostics is None:
    raise SystemExit("missing diagnostics endpoint or route")

product_data = json.loads(products)
if isinstance(product_data, dict):
    candidates = product_data.get("products") or product_data.get("items") or product_data.get("data")
else:
    candidates = product_data
if not isinstance(candidates, list) or not candidates:
    raise SystemExit("/api/products returned no product list")

first = candidates[0]
product_id = first.get("id") or first.get("productId") or first.get("sku")
if not product_id:
    raise SystemExit("first product has no id")

login_payloads = [
    {"username": "shopper", "password": "password"},
    {"email": "shopper@example.com", "password": "password"},
]
logged_in = False
for path in ("/auth/login", "/api/auth/login"):
    for payload in login_payloads:
        try:
            fetch(path, method="POST", payload=payload, expected=(200, 201))
            logged_in = True
            break
        except Exception:
            continue
    if logged_in:
        break
if not logged_in:
    raise SystemExit("shopper login failed on /auth/login and /api/auth/login")

cart_payload = {"productId": product_id, "quantity": 1}
cart_body = None
for method, path in (
    ("POST", "/cart/items"),
    ("POST", "/api/cart/items"),
    ("POST", "/cart"),
    ("POST", "/api/cart"),
    ("PATCH", f"/cart/items/{product_id}"),
    ("PATCH", f"/api/cart/items/{product_id}"),
):
    try:
        cart_body = fetch(path, method=method, payload=cart_payload, expected=(200, 201))
        break
    except Exception:
        continue
if cart_body is None:
    raise SystemExit("cart add API did not accept a product")

wishlist_body = None
for path in ("/wishlist/items", "/api/wishlist/items"):
    try:
        wishlist_body = fetch(path, method="POST", payload={"productId": product_id}, expected=(200, 201))
        break
    except Exception:
        continue
if wishlist_body is None:
    raise SystemExit("wishlist add API did not accept a product")

checkout_payload = {
    "shippingAddress": {
        "name": "Test Shopper",
        "line1": "1 Test Way",
        "city": "Seattle",
        "state": "WA",
        "postalCode": "98101",
        "country": "US",
    },
    "payment": {"cardNumber": "4111111111111111", "expiry": "12/30", "cvv": "123"},
}
checkout_body = None
for path in ("/checkout", "/api/checkout"):
    try:
        checkout_body = fetch(path, method="POST", payload=checkout_payload, expected=(200, 201))
        break
    except Exception:
        continue
if checkout_body is None:
    raise SystemExit("checkout API did not create an order")

orders = None
for path in ("/orders", "/api/orders"):
    try:
        orders = fetch(path)
        break
    except Exception:
        continue
if orders is None:
    raise SystemExit("orders API unavailable after checkout")

for path in (
    "/notifications",
    "/seller/products",
    "/seller/metrics",
    "/admin/moderation",
):
    try:
        fetch(path)
    except Exception:
        try:
            fetch("/api" + path)
        except Exception as exc:
            raise SystemExit(f"required API unavailable: {path}") from exc

print(json.dumps({
    "health": bool(health),
    "products": len(candidates),
    "api_products": bool(api_products),
    "diagnostics": bool(diagnostics),
    "cart_add": bool(cart_body),
    "wishlist_add": bool(wishlist_body),
    "checkout": bool(checkout_body),
    "orders": bool(orders),
}))
PY

echo "public acceptance passed"
