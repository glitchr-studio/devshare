"""The local services of the harness host.

Each one answers with its own name and the Host header it received, so a test
can tell which service a hostname and a port really led to.
"""

import functools
import hashlib
import ssl
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

# name, address, port, tls
SERVICES = [
    ("shop", "127.0.0.2", 80, False),
    ("shop-tls", "127.0.0.2", 443, True),
    ("vite", "127.0.0.2", 5173, False),
    ("api", "127.0.0.2", 8080, False),
    ("database", "127.0.0.2", 5432, False),
    ("admin", "127.0.0.3", 80, False),
    ("grafana", "127.0.0.4", 3000, False),
]


@functools.lru_cache(maxsize=4)
def blob(size):
    """A body no cache or proxy could invent: a hash chain.

    Kept once computed: computing it takes longer than sending it, and a
    test timing a download must not time this.
    """
    out, block = bytearray(), b"devshare"
    while len(out) < size:
        block = hashlib.sha256(block).digest()
        out += block
    return bytes(out[:size])


def handler(name):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def reply(self, body):
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def until_close(self, body):
            """No length: the body ends when the connection does."""
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            self.close_connection = True

        def do_GET(self):
            url = urlparse(self.path)
            if url.path == "/until-close":
                self.until_close(blob(int(parse_qs(url.query)["size"][0])))
            elif url.path == "/blob":
                self.reply(blob(int(parse_qs(url.query)["size"][0])))
            else:
                host = self.headers.get("Host", "")
                self.reply(f"service={name} host={host} path={url.path}\n".encode())

        def do_POST(self):
            if self.path == "/echo-hash":
                # No length either way: the request ends when the client
                # closes its side, the answer when the server closes.
                body = self.rfile.read()
                self.until_close(f"sha256={hashlib.sha256(body).hexdigest()}\n".encode())
                return
            body = self.rfile.read(int(self.headers["Content-Length"]))
            self.reply(f"service={name} sha256={hashlib.sha256(body).hexdigest()}\n".encode())

        def log_message(self, *_):
            pass

    return Handler


def main():
    certificate, key = sys.argv[1], sys.argv[2]
    for name, address, port, tls in SERVICES:
        server = ThreadingHTTPServer((address, port), handler(name))
        if tls:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(certificate, key)
            server.socket = context.wrap_socket(server.socket, server_side=True)
        threading.Thread(target=server.serve_forever, daemon=True).start()
    print("services ready", flush=True)
    threading.Event().wait()


if __name__ == "__main__":
    main()
