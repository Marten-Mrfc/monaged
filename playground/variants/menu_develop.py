"""The menu. Prices are whole cents (ints), never floats."""

from dataclasses import dataclass


@dataclass(frozen=True)
class Item:
    id: str
    name: str
    price: int  # cents
    tags: tuple[str, ...] = ()


MENU = [
    Item("tomato-soup", "Tomato soup", 450, ("vegan",)),
    Item("club", "Club sandwich", 795),
    Item("falafel", "Falafel wrap", 725, ("vegan",)),
    Item("caesar", "Caesar salad", 850),
    Item("brownie", "Brownie", 295, ("vegetarian",)),
    Item("lemonade", "Fresh lemonade", 325, ("vegan",)),
]


def find(item_id: str) -> Item | None:
    return next((item for item in MENU if item.id == item_id), None)


def with_tag(tag: str) -> list[Item]:
    return [item for item in MENU if tag in item.tags]


def format_price(cents: int) -> str:
    """795 -> "€ 7,95" (Dutch notation)."""
    return f"€ {cents // 100},{cents % 100:02d}"
