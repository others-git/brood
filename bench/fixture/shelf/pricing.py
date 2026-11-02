from .codes import percent_for

TAX_BP = 800  # 8.00% in basis points


def apply_discount(cents, percent):
    return cents * (100 - percent) // 100


def tax(cents, rate_bp=TAX_BP):
    return (cents * rate_bp + 5000) // 10000


def invoice_total(lines, code=None):
    """lines: list of (Item, quantity). Returns total in cents."""
    subtotal = sum(item.price_cents * n for item, n in lines)
    discounted = apply_discount(subtotal, percent_for(code))
    return discounted + tax(discounted)
