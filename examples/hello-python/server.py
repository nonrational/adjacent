#!/usr/bin/env python3
import os
import signal
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer


port = int(os.environ.get("PORT", "0"))
if not port:
    print("PORT not set — Adjacent should inject it", file=sys.stderr)
    sys.exit(1)


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self._start = time.monotonic()

    def do_GET(self):
        if self.path == "/healthz":
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(b"ok\n")
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(
            f"Hello from Python {sys.version_info.major}.{sys.version_info.minor} on :{port}\n".encode()
        )

    def log_request(self, code="-", size="-"):
        ms = (time.monotonic() - self._start) * 1000
        print(
            f"[hello-python] {self.command} {self.path} {code} {ms:.1f}ms",
            file=sys.stderr,
        )

    def log_message(self, format, *args):
        print(f"[hello-python] {format % args}", file=sys.stderr)


server = HTTPServer(("127.0.0.1", port), Handler)


def shutdown(signum, _frame):
    print(f"hello-python received signal {signum}, shutting down", file=sys.stderr)
    server.shutdown()


signal.signal(signal.SIGTERM, shutdown)
signal.signal(signal.SIGINT, shutdown)

print(f"hello-python listening on 127.0.0.1:{port}", file=sys.stderr)
server.serve_forever()
