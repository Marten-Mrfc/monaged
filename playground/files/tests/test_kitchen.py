"""A flaky test on purpose: Monaged's Run tab ("Run 3×") should catch it."""

import random
import unittest


class KitchenTests(unittest.TestCase):
    def test_kitchen_answers_in_time(self):
        # A pretend kitchen that is too slow about one time in three.
        latency_ms = random.choice([120, 180, 950])
        self.assertLess(latency_ms, 500, f"the kitchen took {latency_ms} ms")


if __name__ == "__main__":
    unittest.main()
