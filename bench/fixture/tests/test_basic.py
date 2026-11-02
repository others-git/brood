import unittest

from shelf.models import Item
from shelf.pricing import apply_discount, invoice_total, tax
from shelf.report import format_inventory
from shelf.store import Store
from shelf.utils import format_money, parse_money


class UtilsTest(unittest.TestCase):
    def test_format(self):
        self.assertEqual(format_money(1299), "$12.99")
        self.assertEqual(format_money(-5), "-$0.05")

    def test_parse(self):
        self.assertEqual(parse_money("$12.9"), 1290)


class StoreTest(unittest.TestCase):
    def setUp(self):
        self.store = Store([Item("X1", "x", 100, 3), Item("X2", "y", 50, 2, "b")])

    def test_total(self):
        self.assertEqual(self.store.total_value_cents(), 400)

    def test_category(self):
        self.assertEqual([i.sku for i in self.store.by_category("b")], ["X2"])

    def test_report(self):
        self.assertIn("TOTAL $4.00", format_inventory(self.store))


class PricingTest(unittest.TestCase):
    def test_discount(self):
        self.assertEqual(apply_discount(1000, 10), 900)

    def test_tax(self):
        self.assertEqual(tax(1000), 80)

    def test_invoice(self):
        lines = [(Item("X1", "x", 1000, 1), 1)]
        self.assertEqual(invoice_total(lines), 1080)
        self.assertEqual(invoice_total(lines, "SAVE10"), 972)
