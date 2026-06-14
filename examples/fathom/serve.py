#!/usr/bin/env python3
"""Static file server that sets the cross-origin isolation headers
SharedArrayBuffer requires (COOP: same-origin, COEP: require-corp).

`python3 -m http.server` does NOT set these, so the threaded WASM build
silently falls back to the single-thread module without them. Run this
instead:  python3 serve.py  [port]
"""
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class Handler(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    print(f"serving cross-origin isolated on http://localhost:{port}")
    ThreadingHTTPServer(("", port), Handler).serve_forever()
