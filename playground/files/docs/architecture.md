# How Lunchbox fits together

```
browser ──HTTP──▶ lunchbox/server.py ──▶ lunchbox/orders.py ──▶ lunchbox/menu.py
                         ▲
agents ──MCP (stdio)── tools/menu_mcp.py ──────────────────────▶ lunchbox/menu.py
```

- The server keeps orders in memory; restarting it clears them.
- Money is whole cents end to end; `format_price` is only for display.
- `docs/screenshot.png` is the menu page, for trying image attachments.
