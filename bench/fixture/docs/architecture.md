# Architecture

- `models.py`: `Item` dataclass.
- `store.py`: `Store`, an in-memory item index with JSON load and save.
- `codes.py`: discount code table and lookup.
- `pricing.py`: discount, tax and invoice totals.
- `report.py`: text output for inventory and invoices.
- `cli.py`: argparse front end. `server.py`: read-only JSON HTTP API.

Money is always integer cents. Tax is in basis points.
