import argparse
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

from .store import Store


def make_handler(store):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/health":
                body = {"ok": True}
            elif self.path == "/summary":
                body = {"count": len(store.all()),
                        "total_value_cents": store.total_value_cents()}
            else:
                self.send_error(404)
                return
            data = json.dumps(body).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *a):
            pass
    return Handler


def main(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("--data", default="data/items.json")
    p.add_argument("--port", type=int, default=8765)
    args = p.parse_args(argv)
    HTTPServer(("127.0.0.1", args.port), make_handler(Store.load(args.data))).serve_forever()


if __name__ == "__main__":
    main()
