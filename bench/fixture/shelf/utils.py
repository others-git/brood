def format_money(cents):
    sign = "-" if cents < 0 else ""
    cents = abs(cents)
    return f"{sign}${cents // 100}.{cents % 100:02d}"


def parse_money(text):
    text = text.strip().lstrip("$")
    dollars, _, frac = text.partition(".")
    frac = (frac + "00")[:2]
    return int(dollars or 0) * 100 + int(frac)
