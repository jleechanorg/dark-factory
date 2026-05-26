#!/usr/bin/env bash
# Public catalog/search/reviews gate for the Amazon benchmark candidate.

set -euo pipefail

WORKDIR="${1:-.}"
cd "$WORKDIR"

PORT="${PORT:-31341}"
BASE_URL="http://127.0.0.1:${PORT}"

fail() {
    echo "CATALOG_FAIL: $*" >&2
    exit 1
}

for file in Makefile package.json firebase.json firestore.rules src/server.js src/public/index.html src/public/app.js src/public/styles.css; do
    [ -f "$file" ] || fail "missing required file: $file"
done

echo "== foundation regression gate =="
bash benchmarks/amazon-clone/scripts/public_foundation.sh .

echo "== catalog static checks =="
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
    "GET /products",
    "POST /products",
    "PATCH /products/:id",
    "archive",
    "restock",
    "GET /reviews",
    "POST /reviews",
    "helpful",
    "report",
    "department",
    "sort",
    "pagination",
    "No products found",
    "quick view",
]
missing = [term for term in required if term.lower() not in source.lower()]
if missing:
    raise SystemExit(f"missing catalog terms: {', '.join(missing)}")
print("catalog static checks passed")
PY

echo "== catalog runtime smoke =="
PORT="$PORT" make run > /tmp/amazon-catalog-run.log 2>&1 &
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
import urllib.parse
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
fetch("/auth/login", method="POST", payload={"username": "seller", "password": "password"}, expected=(200, 201))

def parse_products(body):
    data = json.loads(body)
    if isinstance(data, list):
        return data
    for key in ("products", "items", "data", "results"):
        value = data.get(key)
        if isinstance(value, list):
            return value
    raise SystemExit("product response did not contain a product list")

products = parse_products(fetch("/products"))
api_products = parse_products(fetch("/api/products"))
if len(products) < 8:
    raise SystemExit(f"expected at least 8 products, got {len(products)}")
first = products[0]
product_id = first.get("id") or first.get("productId") or first.get("sku")
if not product_id:
    raise SystemExit("first product missing id")
for field in ("title", "brand", "department", "description"):
    if not first.get(field):
        raise SystemExit(f"first product missing {field}")
if first.get("priceCents") is None and first.get("price") is None:
    raise SystemExit("first product missing price")

department = urllib.parse.quote(str(first.get("department")))
search = urllib.parse.quote(str(first.get("title", "").split()[0] or first.get("brand")))
parse_products(fetch(f"/products?department={department}"))
parse_products(fetch(f"/products?search={search}"))
parse_products(fetch("/products?sort=price_asc&page=1&pageSize=4"))
empty = fetch("/products?search=zzzz_no_products_expected")
if "no products" not in empty.lower() and "[]" not in empty:
    raise SystemExit("empty search did not expose a no-products state")

detail = json.loads(fetch(f"/products/{product_id}"))
if isinstance(detail, dict) and "product" in detail:
    detail = detail["product"]
for field in ("title", "description", "department"):
    if not detail.get(field):
        raise SystemExit(f"product detail missing {field}")

created = json.loads(fetch("/products", method="POST", payload={
    "title": "Foundation Seller Test Product",
    "brand": "Factory",
    "department": "Tools",
    "description": "Created by catalog public gate",
    "priceCents": 1999,
    "stockOnHand": 12,
}, expected=(200, 201)))
created_product = created.get("product") if isinstance(created, dict) else created
created_id = created_product.get("id") or created_product.get("productId")
if not created_id:
    raise SystemExit("product create did not return id")
fetch(f"/products/{created_id}", method="PATCH", payload={"priceCents": 2099}, expected=(200, 204))
fetch(f"/products/{created_id}/restock", method="POST", payload={"quantity": 5}, expected=(200, 204))
fetch(f"/products/{created_id}/archive", method="POST", expected=(200, 204))

reviews = fetch(f"/products/{product_id}/reviews")
if "review" not in reviews.lower() and "[]" not in reviews:
    raise SystemExit("product reviews endpoint returned unexpected body")
review_body = json.loads(fetch("/reviews", method="POST", payload={
    "productId": product_id,
    "rating": 5,
    "title": "Works well",
    "body": "Catalog public gate review",
}, expected=(200, 201)))
review = review_body.get("review") if isinstance(review_body, dict) else review_body
review_id = review.get("id") or review.get("reviewId")
if not review_id:
    raise SystemExit("review create did not return id")
fetch(f"/reviews/{review_id}/helpful", method="POST", expected=(200, 204))
fetch(f"/reviews/{review_id}/report", method="POST", payload={"reason": "public gate probe"}, expected=(200, 204))
fetch("/api/reviews")

print(json.dumps({
    "products": len(products),
    "api_products": len(api_products),
    "product_id": product_id,
    "created_product": created_id,
    "review": review_id,
}))
PY

echo "catalog public acceptance passed"
