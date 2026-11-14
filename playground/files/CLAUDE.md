# Lunchbox

A small office-lunch ordering app, made to try Monaged on. Python 3 standard
library only: no packages to install.

## Layout

- `lunchbox/menu.py`: the menu and `format_price`.
- `lunchbox/orders.py`: orders, coupons and totals.
- `lunchbox/server.py`: the web app (`python3 -m lunchbox.server`, port 8765).
- `tools/menu_mcp.py`: an MCP server with the menu (`list_menu`, `price_of`).
- `tests/`: unittest tests (`python3 -m unittest -v`).

## Rules

- Prices are whole cents (`int`), never floats. Format them only for display,
  with `format_price`.
- Keep it standard library only.
- Every change to `orders.py` comes with a test in `tests/test_orders.py`.
- Run the tests before you say you're done.
