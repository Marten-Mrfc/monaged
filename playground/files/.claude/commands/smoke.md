---
description: Run the tests and check the web app answers.
---

Run `python3 -m unittest -v`. Then start `python3 -m lunchbox.server` in the
background, fetch `http://localhost:8765/api/menu` with `curl -s`, stop the
server, and report: tests passed or failed (which), and whether the menu API
answered.
