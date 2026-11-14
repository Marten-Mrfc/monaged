---
name: add-menu-item
description: Add a dish or drink to the Lunchbox menu, with its price in cents, tags and a test. Use when asked to add something to the menu.
---

# Add a menu item

1. Add an `Item` to `MENU` in `lunchbox/menu.py`: a short kebab-case id, the
   name as shown, the price in whole cents, and tags (`vegan`, `vegetarian`)
   where they apply.
2. Check the id is unique (`tests/test_menu.py` checks it too).
3. If it has a tag, update `test_vegan` (or add a test for the new tag).
4. Run `python3 -m unittest -v` and fix anything that fails.
5. Say what you added and its formatted price.
