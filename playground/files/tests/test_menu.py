import unittest

from lunchbox.menu import MENU, find, format_price, with_tag


class MenuTests(unittest.TestCase):
    def test_every_price_is_whole_cents(self):
        for item in MENU:
            self.assertIsInstance(item.price, int, item.id)
            self.assertGreater(item.price, 0, item.id)

    def test_ids_are_unique(self):
        ids = [item.id for item in MENU]
        self.assertEqual(len(ids), len(set(ids)))

    def test_find(self):
        self.assertEqual(find("club").name, "Club sandwich")
        self.assertIsNone(find("pizza"))

    def test_vegan(self):
        self.assertEqual({i.id for i in with_tag("vegan")}, {"tomato-soup", "falafel", "lemonade"})

    def test_format_price(self):
        self.assertEqual(format_price(795), "€7.95")
        self.assertEqual(format_price(5), "€0.05")


if __name__ == "__main__":
    unittest.main()
