#!/usr/bin/env python3
"""A tiny MCP server (stdio, newline-delimited JSON-RPC) with the Lunchbox menu.

Tools: list_menu (optionally by tag) and price_of. Monaged's playground adds
it under Project settings → MCP servers, so agents in this project can call it.
"""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

from lunchbox.menu import MENU, find, format_price, with_tag  # noqa: E402

TOOLS = [
    {
        "name": "list_menu",
        "description": "Everything on the Lunchbox menu with its price. Optionally only items with a tag (vegan, vegetarian).",
        "inputSchema": {"type": "object", "properties": {"tag": {"type": "string"}}},
    },
    {
        "name": "price_of",
        "description": "The price of one menu item, by id (e.g. club) or name.",
        "inputSchema": {"type": "object", "properties": {"item": {"type": "string"}}, "required": ["item"]},
    },
]


def call(name: str, args: dict) -> str:
    if name == "list_menu":
        items = with_tag(args["tag"]) if args.get("tag") else MENU
        return "\n".join(f"{i.id}: {i.name}, {format_price(i.price)}" for i in items) or "Nothing with that tag."
    if name == "price_of":
        wanted = str(args.get("item", "")).lower()
        item = find(wanted) or next((i for i in MENU if i.name.lower() == wanted), None)
        return f"{item.name}: {format_price(item.price)}" if item else f"No item called {wanted!r}."
    raise ValueError(f"Unknown tool {name}")


def handle(message: dict) -> dict | None:
    method, msg_id = message.get("method"), message.get("id")
    if msg_id is None:
        return None  # a notification
    try:
        if method == "initialize":
            version = message.get("params", {}).get("protocolVersion", "2024-11-05")
            result = {"protocolVersion": version, "capabilities": {"tools": {}}, "serverInfo": {"name": "lunchbox-menu", "version": "1.0"}}
        elif method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            params = message.get("params", {})
            result = {"content": [{"type": "text", "text": call(params.get("name"), params.get("arguments") or {})}]}
        elif method == "ping":
            result = {}
        else:
            return {"jsonrpc": "2.0", "id": msg_id, "error": {"code": -32601, "message": f"Unknown method {method}"}}
    except Exception as err:  # report, don't crash the server
        return {"jsonrpc": "2.0", "id": msg_id, "result": {"content": [{"type": "text", "text": str(err)}], "isError": True}}
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def main() -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            reply = handle(json.loads(line))
        except json.JSONDecodeError:
            continue
        if reply is not None:
            sys.stdout.write(json.dumps(reply) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
