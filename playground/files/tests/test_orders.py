import unittest

from lunchbox.orders import Order, subtotal, total


class OrderTests(unittest.TestCase):
    def test_subtotal(self):
        order = Order("Sam")
        order.add("club", 2)
        order.add("lemonade")
        self.assertEqual(subtotal(order), 2 * 795 + 325)

    def test_unknown_item(self):
        with self.assertRaises(ValueError):
            Order("Sam").add("pizza")

    def test_quantity_must_be_positive(self):
        with self.assertRaises(ValueError):
            Order("Sam").add("club", 0)

    def test_coupon_applies_to_the_whole_order(self):
        order = Order("Robin", coupon="LUNCH10")
        order.add("caesar")  # 850
        order.add("brownie", 2)  # 590
        # 10% off 1440 is 144.
        self.assertEqual(total(order), 1440 - 144)

    def test_unknown_coupon_changes_nothing(self):
        order = Order("Robin", coupon="FREELUNCH")
        order.add("falafel")
        self.assertEqual(total(order), 725)


if __name__ == "__main__":
    unittest.main()
