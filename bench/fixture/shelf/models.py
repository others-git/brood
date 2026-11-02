from dataclasses import dataclass, asdict


@dataclass
class Item:
    sku: str
    name: str
    price_cents: int
    qty: int
    category: str = "general"

    def value_cents(self):
        return self.price_cents * self.qty

    def to_dict(self):
        return asdict(self)
