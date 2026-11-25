"""Serve the browser jobs example with shared Wasm memory enabled."""
import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


def server_for(site, isolated, port=0):
    class Handler(SimpleHTTPRequestHandler):
        def end_headers(self):
            if isolated:
                self.send_header("Cross-Origin-Opener-Policy", "same-origin")
                self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
            super().end_headers()

        def log_message(self, *args):
            pass

    return ThreadingHTTPServer(("127.0.0.1", port), partial(Handler, directory=site))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("site")
    parser.add_argument("--port", type=int, default=8000)
    args = parser.parse_args()
    with server_for(args.site, True, args.port) as server:
        print(f"http://127.0.0.1:{server.server_port}/", flush=True)
        server.serve_forever()
