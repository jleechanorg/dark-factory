#!/usr/bin/env bash
# Public cart/wishlist/addresses gate for the Amazon benchmark candidate.

set -euo pipefail

WORKDIR="${1:-.}"
cd "$WORKDIR"

PORT="${PORT:-31343}"
BASE_URL="http://127.0.0.1:${PORT}"

fail() {
    echo "CART_FAIL: $*" >&2
    exit 1
}

for file in Makefile package.json firebase.json firestore.rules src/server.js src/public/index.html src/public/app.js src/public/styles.css; do
    [ -f "$file" ] || fail "missing required file: $file"
done

echo "== catalog regression gate =="
bash benchmarks/amazon-clone/scripts/public_catalog.sh .

echo "== cart static checks =="
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
    "GET /cart",
    "POST /cart/items",
    "PATCH /cart/items/:productId",
    "DELETE /cart/items/:productId",
    "save-for-later",
    "apply-coupon",
    "GET /wishlist",
    "POST /wishlist/items",
    "GET /addresses",
    "POST /addresses",
    "defaultAddress",
    "subtotalCents",
    "grandTotalCents",
    "quantity",
    "stock",
]
missing = [term for term in required if term.lower() not in source.lower()]
if missing:
    raise SystemExit(f"missing cart terms: {', '.join(missing)}")
print("cart static checks passed")
PY

echo "== cart runtime smoke =="
PORT="$PORT" make run > /tmp/amazon-cart-run.log 2>&1 &
server_pid=$!
cleanup() {
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
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

fetch("/seed/reset", method="POST", expected=(200, 201, 204))
fetch("/auth/login", method="POST", payload={"username": "shopper", "password": "password"}, expected=(200, 201))

products_data = json.loads(fetch("/products"))
products = products_data if isinstance(products_data, list) else products_data.get("products") or products_data.get("items") or products_data.get("data")
if not isinstance(products, list) or not products:
    raise SystemExit("no products available for cart smoke")
product_id = products[0].get("id") or products[0].get("productId") or products[0].get("sku")
if not product_id:
    raise SystemExit("first product has no id")

def parse_obj(body):
    data = json.loads(body)
    return data

cart = parse_obj(fetch("/cart"))
if not isinstance(cart, dict):
    raise SystemExit("cart response must be an object")
fetch("/api/cart")

added = parse_obj(fetch("/cart/items", method="POST", payload={"productId": product_id, "quantity": 2}, expected=(200, 201)))
if "items" not in json.dumps(added).lower():
    raise SystemExit("cart add response did not contain items")
updated = parse_obj(fetch(f"/cart/items/{product_id}", method="PATCH", payload={"quantity": 3}, expected=(200, 204)))
updated_text = json.dumps(updated).lower()
if "3" not in updated_text and updated:
    raise SystemExit("cart update did not reflect quantity")
fetch(f"/api/cart/items/{product_id}", method="PATCH", payload={"quantity": 1}, expected=(200, 204))

coupon = fetch("/cart/apply-coupon", method="POST", payload={"code": "SAVE10"}, expected=(200, 201, 204, 400))
save_later = fetch("/cart/save-for-later", method="POST", payload={"productId": product_id}, expected=(200, 201, 204))
fetch(f"/cart/items/{product_id}", method="DELETE", expected=(200, 204))
fetch("/cart/items", method="POST", payload={"productId": "missing-product", "quantity": 1}, expected=(400, 404))
fetch("/cart/items", method="POST", payload={"productId": product_id, "quantity": -1}, expected=(400,))

wishlist = fetch("/wishlist")
fetch("/api/wishlist")
added_wishlist = fetch("/wishlist/items", method="POST", payload={"productId": product_id}, expected=(200, 201, 204))
fetch(f"/wishlist/items/{product_id}", method="DELETE", expected=(200, 204))
fetch("/api/wishlist/items", method="POST", payload={"productId": product_id}, expected=(200, 201, 204))

address_payload = {
    "recipient": "Test Shopper",
    "street": "1 Test Way",
    "unit": "Apt 2",
    "city": "Seattle",
    "region": "WA",
    "postalCode": "98101",
    "country": "US",
    "phone": "2065550100",
}
addresses_before = fetch("/addresses")
created = parse_obj(fetch("/addresses", method="POST", payload=address_payload, expected=(200, 201)))
address = created.get("address") if isinstance(created, dict) else created
address_id = address.get("id") or address.get("addressId")
if not address_id:
    raise SystemExit("address create did not return id")
fetch(f"/addresses/{address_id}", method="PATCH", payload={"unit": "Suite 5"}, expected=(200, 204))
fetch(f"/addresses/{address_id}/default", method="POST", expected=(200, 204))
fetch("/api/addresses")
fetch(f"/addresses/{address_id}", method="DELETE", expected=(200, 204))

print(json.dumps({
    "product_id": product_id,
    "cart_add": bool(added),
    "coupon_probe": bool(coupon) or coupon == "",
    "save_later": bool(save_later) or save_later == "",
    "wishlist": bool(wishlist) or wishlist == "",
    "wishlist_add": bool(added_wishlist) or added_wishlist == "",
    "addresses_before": bool(addresses_before) or addresses_before == "",
    "address_id": address_id,
}))
PY

echo "cart public acceptance passed"
