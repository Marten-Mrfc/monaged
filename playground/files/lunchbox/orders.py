"""Orders and what they cost."""

from dataclasses import dataclass, field

from .menu import find

# Percent off the whole order.
COUPONS = {"LUNCH10": 10, "FRIDAY25": 25}


@dataclass
class Order:
    name: str
    lines: list[tuple[str, int]] = field(default_factory=list)  # (item id, quantity)
    coupon: str | None = None

    def add(self, item_id: str, quantity: int = 1) -> None:
        if find(item_id) is None:
            raise ValueError(f"No such item: {item_id}")
        if quantity < 1:
            raise ValueError("Quantity must be at least 1")
        self.lines.append((item_id, quantity))


def line_total(item_id: str, quantity: int) -> int:
    return find(item_id).price * quantity


def subtotal(order: Order) -> int:
    return sum(line_total(item_id, quantity) for item_id, quantity in order.lines)


def discount(order: Order) -> int:
    """What the coupon takes off, in cents (rounded to the nearest cent)."""
    percent = COUPONS.get((order.coupon or "").upper(), 0)
    if not order.lines or not percent:
        return 0
    first = line_total(*order.lines[0])
    return round(first * percent / 100)


def total(order: Order) -> int:
    return subtotal(order) - discount(order)
