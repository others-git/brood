from .pricing import invoice_total
from .utils import format_money


def format_inventory(store):
    rows = [f"{i.sku:<8} {i.name:<20} {i.qty:>4} {format_money(i.price_cents):>9}"
            for i in store.all()]
    rows.append(f"TOTAL {format_money(store.total_value_cents())}")
    return "\n".join(rows)


def format_invoice(lines, code=None):
    out = [f"{item.sku} x{n} {format_money(item.price_cents * n)}" for item, n in lines]
    out.append(f"DUE {format_money(invoice_total(lines, code))}")
    return "\n".join(out)
