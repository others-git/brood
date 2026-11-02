import json

from .models import Item


class Store:
    def __init__(self, items=None):
        self._items = {i.sku: i for i in (items or [])}

    @classmethod
    def load(cls, path):
        with open(path) as f:
            return cls([Item(**d) for d in json.load(f)])

    def save(self, path):
        with open(path, "w") as f:
            json.dump([i.to_dict() for i in self.all()], f, indent=2)

    def add(self, item):
        self._items[item.sku] = item

    def get(self, sku):
        return self._items[sku]

    def all(self):
        return sorted(self._items.values(), key=lambda i: i.sku)

    def by_category(self, category):
        return [i for i in self.all() if i.category == category]

    def total_value_cents(self):
        return sum(i.value_cents() for i in self.all())
