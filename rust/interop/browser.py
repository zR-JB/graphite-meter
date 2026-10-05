"""Drive Chromium through the Rust server's HTTP/3 and WebTransport routes and session close codes.

    python3 rust/interop/browser.py --chromium BINARY [--server BINARY]

Chromium must fetch over HTTP/3, use WebTransport datagrams and streams, and see the close code and reason of
a session the server ends at its lifetime and when it stops. The server shards HTTP/3 over this host's runtime
threads. Without --server it builds the static musl server with the ci profile.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.server
import json
import os
import signal
import socket
import ssl
import subprocess
import threading
from pathlib import Path
from typing import Any, Callable

from fixture import ROOT, Fixture, Server, build_server, planned_endpoints, udp_sockets

PAGE = (Path(__file__).parent / "browser.html").read_bytes()
MIB = 1024 * 1024
CHECKS: dict[str, Callable[[Any], bool]] = {
    "download": lambda d: d == {"bytes": MIB, "protocol": "h3"},
    "upload": lambda d: d == {"bytes": MIB, "protocol": "h3"},
    "wt_ping": lambda d: isinstance(d, str) and d.startswith("PONG,7,"),
    "wt_download": lambda d: d == 65536,
    "wt_upload": lambda d: isinstance(d, int) and d >= 65536,
    "wt_close": lambda d: d == {"closeCode": 2, "reason": "lifetime"},
    "wt_shutdown": lambda d: d == {"closeCode": 4, "reason": "shutdown"},
}


class Pages(http.server.ThreadingHTTPServer):
    """Serves the page over HTTPS, stops the measured server on request and keeps the page's report."""

    def __init__(self, context: ssl.SSLContext, measured: Server) -> None:
        super().__init__(("127.0.0.1", 0), Handler)
        self.context, self.measured = context, measured
        self.report: dict[str, Any] = {}
        self.received = threading.Event()

    def get_request(self) -> tuple[socket.socket, Any]:
        # The handshake runs in the handler thread, so a stalled client cannot block accept.
        connection, address = self.socket.accept()
        return self.context.wrap_socket(connection, server_side=True, do_handshake_on_connect=False), address


class Handler(http.server.BaseHTTPRequestHandler):
    server: Pages

    def do_GET(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path == "/stop":
            assert self.server.measured.process is not None
            self.server.measured.process.send_signal(signal.SIGTERM)
        else:
            self.server.report = json.loads(body)
            self.server.received.set()
        self.send_response(204)
        self.end_headers()

    def log_message(self, format: str, *args: Any) -> None:
        pass


def chromium(binary: str, fixture: Fixture, pages: Pages, server: Server) -> dict[str, Any]:
    public_key = subprocess.run(["openssl", "x509", "-in", str(fixture.cert), "-pubkey", "-noout"], check=True,
                                capture_output=True).stdout
    spki = subprocess.run(["openssl", "pkey", "-pubin", "-outform", "der"], input=public_key, check=True,
                          capture_output=True).stdout
    certificate = hashlib.sha256(ssl.PEM_cert_to_DER_cert(fixture.cert.read_text())).digest()
    page = (f"https://127.0.0.1:{pages.server_address[1]}/?base={server.origin('http3')}"
            f"&hash={','.join(map(str, certificate))}")
    command = [
        binary, "--headless=new", "--no-sandbox", "--no-first-run", "--disable-gpu",
        f"--user-data-dir={fixture.directory / 'chromium'}",
        f"--ignore-certificate-errors-spki-list={base64.b64encode(hashlib.sha256(spki).digest()).decode()}",
        f"--origin-to-force-quic-on=127.0.0.1:{server.h3}", page,
    ]
    with (fixture.directory / "chromium.log").open("w") as log:
        browser = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                   env=os.environ | {"HOME": str(fixture.directory)}, start_new_session=True)
    try:
        if not pages.received.wait(90):
            raise TimeoutError("the page reported nothing")
        return pages.report
    finally:
        os.killpg(browser.pid, signal.SIGKILL)
        browser.wait()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--chromium", required=True, help="the Chromium or Chrome for Testing binary")
    parser.add_argument("--server", type=Path, help="prebuilt server binary")
    args = parser.parse_args()
    binary = args.server.resolve() if args.server else build_server(ROOT, "ci")
    fixture = Fixture("browser-")
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(fixture.cert, fixture.key)
    settings = {"GM_MAX_OPERATION_DURATION": "2s", "GM_MAX_SESSION_DURATION": "3s"}
    with fixture.server(binary, "chromium", settings) as server:
        planned, sockets = planned_endpoints(), udp_sockets(server.h3)
        if sockets != planned:
            raise RuntimeError(f"{sockets} HTTP/3 sockets, {planned} planned")
        pages = Pages(context, server)
        threading.Thread(target=pages.serve_forever, daemon=True).start()
        try:
            report = chromium(args.chromium, fixture, pages, server)
        finally:
            pages.shutdown()
    (fixture.directory / "chromium.json").write_text(json.dumps(report, indent=1))
    print(f"chromium on {sockets} HTTP/3 endpoints: {report['ua']}", flush=True)
    failed = []
    for name, accept in CHECKS.items():
        check = report["checks"].get(name, {"ok": False, "detail": "missing"})
        passed = check["ok"] and accept(check["detail"])
        print(f"  {'pass' if passed else 'FAIL'} {name}: {json.dumps(check['detail'])}", flush=True)
        if not passed:
            failed.append(name)
    if failed:
        raise SystemExit(f"failed: {', '.join(failed)}")


if __name__ == "__main__":
    main()
