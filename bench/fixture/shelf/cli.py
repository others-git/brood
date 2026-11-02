import argparse
import sys

from .report import format_inventory, format_invoice
from .store import Store


def build_parser():
    p = argparse.ArgumentParser(prog="shelf")
    p.add_argument("--data", default="data/items.json")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("list")
    sub.add_parser("total")
    inv = sub.add_parser("invoice")
    inv.add_argument("sku_qty", nargs="+", help="SKU:QTY")
    inv.add_argument("--code")
    return p


def main(argv=None):
    args = build_parser().parse_args(argv)
    store = Store.load(args.data)
    if args.cmd == "list":
        print(format_inventory(store))
    elif args.cmd == "total":
        print(store.total_value_cents())
    elif args.cmd == "invoice":
        lines = []
        for part in args.sku_qty:
            sku, _, n = part.partition(":")
            lines.append((store.get(sku), int(n or 1)))
        print(format_invoice(lines, args.code))
    return 0


if __name__ == "__main__":
    sys.exit(main())
