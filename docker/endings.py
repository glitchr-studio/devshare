"""How connections end, seen from a program on the guest.

curl hides all of this: it stops reading at Content-Length and never closes
its own side first. Each case prints one line, `ok` or what went wrong.

    python3 endings.py http://shop.test
"""

import hashlib
import socket
import sys
import time
from urllib.parse import urlparse


def connect(url):
    target = urlparse(url)
    connection = socket.create_connection((target.hostname, target.port or 80), timeout=10)
    return connection, target.hostname


def read_all(connection):
    chunks = []
    while chunk := connection.recv(65536):
        chunks.append(chunk)
    return b"".join(chunks)


def body_of(response):
    return response.split(b"\r\n\r\n", 1)[1]


def server_closes_first(url, size):
    """A response with no length: its end is the server closing."""
    connection, host = connect(url)
    connection.sendall(f"GET /until-close?size={size} HTTP/1.0\r\nHost: {host}\r\n\r\n".encode())
    started = time.monotonic()
    body = body_of(read_all(connection))
    elapsed = time.monotonic() - started
    connection.close()
    assert len(body) == size, f"{len(body)} bytes of {size}"
    assert elapsed < 5, f"the end came after {elapsed:.1f}s"


def client_closes_its_side_first(url, size):
    """The client says it has nothing more to send, then reads the answer."""
    connection, host = connect(url)
    payload = bytes(range(256)) * (size // 256)
    connection.sendall(f"POST /echo-hash HTTP/1.0\r\nHost: {host}\r\n\r\n".encode() + payload)
    connection.shutdown(socket.SHUT_WR)
    body = body_of(read_all(connection)).decode()
    connection.close()
    wanted = hashlib.sha256(payload).hexdigest()
    assert wanted in body, f"answer was {body[:80]!r}"


def client_leaves_abruptly(url):
    """Asks for a large answer and leaves without reading it."""
    connection, host = connect(url)
    connection.sendall(f"GET /until-close?size=4194304 HTTP/1.0\r\nHost: {host}\r\n\r\n".encode())
    connection.recv(1024)
    connection.close()


CASES = [
    ("a short answer ended by the server", lambda url: server_closes_first(url, 64)),
    ("a 4 MiB answer ended by the server", lambda url: server_closes_first(url, 4 * 1024 * 1024)),
    ("the client closes its side, then reads", lambda url: client_closes_its_side_first(url, 256 * 1024)),
    ("the client leaves in the middle of an answer", client_leaves_abruptly),
    ("and the next connection is fine", lambda url: server_closes_first(url, 64)),
]

if __name__ == "__main__":
    for name, case in CASES:
        try:
            case(sys.argv[1])
            print(f"{name}: ok", flush=True)
        except Exception as error:  # noqa: BLE001 - every failure is a result here
            print(f"{name}: {type(error).__name__}: {error}", flush=True)
