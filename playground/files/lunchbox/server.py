"""The Lunchbox web app: python3 -m lunchbox.server, then open http://localhost:8765."""

import html
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from . import orders
from .menu import MENU, format_price

PORT = int(os.environ.get("PORT", "8765"))
ORDERS: list[orders.Order] = []

PAGE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Lunchbox</title>
<style>
  body {{ font: 15px/1.5 system-ui, sans-serif; margin: 0; background: #f6f5f2; color: #222; }}
  header {{ background: #2f3e46; color: #fff; padding: 16px 32px; font-size: 20px; }}
  main {{ max-width: 720px; margin: 24px auto; padding: 0 16px; }}
  .item {{ display: flex; justify-content: space-between; background: #fff; border: 1px solid #e3e0da;
           border-radius: 8px; padding: 12px 16px; margin-bottom: 8px; }}
  .tags {{ color: #6b7d5c; font-size: 12px; margin-left: 8px; }}
  .orders {{ margin-top: 32px; color: #555; }}
</style>
</head>
<body>
<header>Lunchbox</header>
<main>
  <h2>Today's menu</h2>
  {items}
  <div class="orders">{orders} order(s) so far today.</div>
</main>
</body>
</html>
"""


def render_page() -> str:
    items = "\n".join(
        f'<div class="item"><span>{html.escape(item.name)}'
        f'<span class="tags">{" ".join(item.tags)}</span></span>'
        f"<span>{format_price(item.price)}</span></div>"
        for item in MENU
    )
    return PAGE.format(items=items, orders=len(ORDERS))


class Handler(BaseHTTPRequestHandler):
    def send(self, status: int, body: str, kind: str = "application/json") -> None:
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", f"{kind}; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        if self.path in ("/", "/index.html"):
            self.send(200, render_page(), "text/html")
        elif self.path == "/api/menu":
            self.send(200, json.dumps([{"id": i.id, "name": i.name, "price": i.price, "tags": i.tags} for i in MENU]))
        elif self.path == "/api/orders":
            self.send(200, json.dumps([{"name": o.name, "lines": o.lines, "total": orders.total(o)} for o in ORDERS]))
        else:
            self.send(404, json.dumps({"error": "not found"}))

    def do_POST(self) -> None:
        if self.path != "/api/orders":
            self.send(404, json.dumps({"error": "not found"}))
            return
        length = int(self.headers.get("Content-Length", "0"))
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
            order = orders.Order(name=body["name"], coupon=body.get("coupon"))
            for item_id, quantity in body.get("lines", []):
                order.add(item_id, quantity)
        except (KeyError, ValueError, TypeError) as err:
            self.send(400, json.dumps({"error": str(err)}))
            return
        ORDERS.append(order)
        self.send(201, json.dumps({"total": orders.total(order), "formatted": format_price(orders.total(order))}))

    def log_message(self, fmt: str, *args) -> None:
        print(f"{self.command} {self.path} -> {args[1] if len(args) > 1 else ''}", flush=True)


def main() -> None:
    server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    print(f"Serving Lunchbox on http://localhost:{PORT}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
