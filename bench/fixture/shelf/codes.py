"""Discount codes."""

DISCOUNTS = {
    "SAVE10": 10,
    "HALF": 50,
    "WELCOME5": 5,
}


def normalize(code):
    return code.strip()


def percent_for(code):
    if not code:
        return 0
    return DISCOUNTS.get(normalize(code), 0)
