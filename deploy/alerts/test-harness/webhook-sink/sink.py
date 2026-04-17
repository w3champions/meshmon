"""Test-harness webhook sink.

Accepts POSTs, logs them, and appends each body as a line to
/data/received.jsonl so the smoke script can assert against it.

Stdlib only — no third-party deps, no builder caching traps.
"""

from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
import sys
import threading

DATA_DIR = "/data"
RECEIVED = os.path.join(DATA_DIR, "received.jsonl")
os.makedirs(DATA_DIR, exist_ok=True)
_lock = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length > 0 else b""
        try:
            payload = json.loads(raw.decode("utf-8")) if raw else {}
        except json.JSONDecodeError:
            payload = {"_raw": raw.decode("utf-8", errors="replace")}
        entry = {"path": self.path, "payload": payload}
        with _lock:
            with open(RECEIVED, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(entry) + "\n")
        sys.stderr.write(f"[sink] {self.path} <- {json.dumps(payload)[:200]}\n")
        self.send_response(204)
        self.end_headers()

    def log_message(self, fmt: str, *args: object) -> None:
        sys.stderr.write("[sink] " + fmt % args + "\n")


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
