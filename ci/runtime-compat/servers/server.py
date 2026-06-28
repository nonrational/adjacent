import os
import platform
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(os.environ["PORT"])
BODY = f"PYTHON {platform.python_version()}\n".encode()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def log_message(self, *args):
        pass


HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
