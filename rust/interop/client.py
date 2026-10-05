"""Run the Rust client against the unchanged Go server.

    python3 rust/interop/client.py

It builds Go's server and the client with the ci profile. The client's report runs complete over WebTransport
streams with datagram latency, HTTPS HTTP/1.1 and HTTP/2 fetch streams, trusting only SSL_CERT_FILE, and fail as
not trusted without it. Against a password-mode Go server a report run exits 1 asking for sign-in, and the ignored
go_signin test signs in once this script approves the page it prints, then runs over the grant.
"""

from __future__ import annotations

import argparse
import http.cookiejar
import re
import ssl
import subprocess
import threading
import urllib.error
import urllib.parse
import urllib.request
from http.client import HTTPMessage
from pathlib import Path
from typing import IO

from fixture import ROOT, Fixture, Server, record

WORKSPACE = ROOT / "rust"
CLIENT = WORKSPACE / "target/ci/graphite-meter-client"
# Building the test also builds the client binary, with the same features.
TEST = ["cargo", "test", "--locked", "--profile", "ci", "-p", "graphite-meter-client", "--test", "go_signin"]
SIGN_IN_TEST = "go_server_approves_a_native_sign_in_for_later_runs"
PASSWORD = "correct horse battery staple"
PASSWORD_HASH = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0"
SIGN_IN = "Sign-in required; run graphite-meter-client in a terminal to sign in."
UNTRUSTED = "Certificate not trusted"
CHALLENGE = re.compile(r"[A-Za-z0-9_-]{43}")
# Label, throughput protocol, throughput transport and latency transport of each report run.
PATHS = (
    ("WebTransport streams with datagram latency", "http3", "webtransport", "webtransport"),
    ("HTTPS HTTP/1.1 fetch streams", "http1", "fetch-stream", "websocket"),
    ("HTTP/2 fetch streams", "http2", "fetch-stream", "websocket"),
)


def go_server(fixture: Fixture, binary: Path, name: str, protected: bool = False) -> Server:
    """Go's server advertising its TLS listeners at their own origins, in password mode when `protected`."""
    server = fixture.server(binary, name)
    public = server.origin("http1-tls")
    server.environment |= {
        "GM_ADVERTISED_NATIVE_ENDPOINTS": "http1-tls,http2,http3", "GM_H1_TLS_PUBLIC_ORIGIN": public,
        "GM_H2_PUBLIC_ORIGIN": server.origin("http2"), "GM_H3_PUBLIC_ORIGIN": server.origin("http3"),
    }
    if protected:
        server.environment |= {"GM_AUTH_MODE": "password", "GM_AUTH_PUBLIC_URL": public,
                               "GM_AUTH_PASSWORD_HASH": PASSWORD_HASH}
    return server


def environment(fixture: Fixture, trusted: bool) -> dict[str, str]:
    """The client's environment without proxies; when `trusted`, SSL_CERT_FILE alone names the fixture's CA."""
    kept = {key: value for key, value in fixture.environment.items()
            if key not in ("SSL_CERT_FILE", "SSL_CERT_DIR") and not key.lower().endswith("_proxy")}
    return (kept | {"SSL_CERT_FILE": str(fixture.ca)}) if trusted else kept


def report(server: Server, protocol: str, throughput: str, latency: str) -> list[str]:
    """A four-stage report run against `server`'s TLS discovery, forced onto one throughput and latency path."""
    durations = [f"--{stage}-duration=3s" for stage in ("download", "upload", "bidirectional")]
    return [str(CLIENT), "--report", "--url", server.origin("http1-tls"),
            "--stages", "latency,download,upload,bidirectional", "--throughput-protocol", protocol,
            "--throughput-transport", throughput, "--latency-transport", latency, "--warmup=250ms",
            "--latency-duration=1s", *durations]


def refused(fixture: Fixture, command: list[str], trusted: bool, line: str, name: str) -> None:
    """The report run exits 1 and prints `line`."""
    result = subprocess.run(command, env=environment(fixture, trusted), capture_output=True, text=True, timeout=60)
    output = result.stdout + result.stderr
    (fixture.directory / f"client-{name}.log").write_text(output)
    if result.returncode != 1 or line not in output:
        raise RuntimeError(f"expected exit 1 with {line!r}, got {result.returncode}:\n{output}")
    print(f"Rust client exited 1: {line}", flush=True)


class Unredirected(urllib.request.HTTPRedirectHandler):
    """Hands a redirect back as the answer."""

    def redirect_request(self, req: urllib.request.Request, fp: IO[bytes], code: int, msg: str,
                         headers: HTTPMessage, newurl: str) -> None:
        return None


def approve(fixture: Fixture, base: str, page: str) -> None:
    """Approves the sign-in at `page` as an operator's browser does: password sign-in, the page, then its form."""
    prefix = f"{base}/auth/cli?challenge="
    challenge = page.removeprefix(prefix)
    if not page.startswith(prefix) or not CHALLENGE.fullmatch(challenge):
        raise RuntimeError(f"not an approval page of {base}: {page!r}")
    cookies = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=ssl.create_default_context(
            cafile=str(fixture.ca))), urllib.request.HTTPCookieProcessor(cookies), Unredirected())

    def request(path: str, form: dict[str, str] | None = None, expected: int = 200) -> None:
        headers = {"Origin": base, "Sec-Fetch-Site": "same-origin"}
        body = None
        if form is not None:
            body = urllib.parse.urlencode(form).encode()
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        try:
            answer = opener.open(urllib.request.Request(base + path, data=body, headers=headers), timeout=10)
        except urllib.error.HTTPError as error:
            answer = error
        with answer:
            answer.read(64 * 1024)
            if answer.status != expected:
                raise RuntimeError(f"{path}: HTTP {answer.status}, expected {expected}")

    def cookie(name: str) -> str:
        value = next((item.value for item in cookies if item.name == name), None)
        if not value:
            raise RuntimeError(f"no {name} cookie")
        return value

    request("/login")
    request("/auth/password", {"csrf": cookie("__Host-gm_login"), "password": PASSWORD}, expected=303)
    cookie("__Host-gm_session")
    request("/auth/cli?" + urllib.parse.urlencode({"challenge": challenge}))
    request("/auth/cli/approve", {"csrf": cookie("__Host-gm_csrf"), "challenge": challenge})
    print("Approved the sign-in through Go's password and approval pages", flush=True)


def sign_in(fixture: Fixture, server: Server) -> None:
    """Runs the ignored sign-in test against `server`, approving the page it prints."""
    base = server.origin("http1-tls")
    log = fixture.directory / "client-sign-in.log"
    command = [*TEST, "--", "--ignored", "--exact", "--nocapture", SIGN_IN_TEST]
    with log.open("w") as output, subprocess.Popen(
            command, cwd=WORKSPACE, env=environment(fixture, True) | {"GM_GO_AUTH_URL": base},
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True) as test:
        watchdog = threading.Timer(300, test.kill)
        watchdog.start()
        try:
            assert test.stdout is not None
            for line in test.stdout:
                output.write(line)
                print(line, end="", flush=True)
                if line.startswith("approve "):
                    approve(fixture, base, line.removeprefix("approve ").strip())
            status = test.wait()
        finally:
            watchdog.cancel()
            if test.poll() is None:
                test.kill()
    if status != 0:
        raise RuntimeError(f"the sign-in test exited {status}; see {log}")


def main() -> None:
    argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter).parse_args()
    fixture = Fixture("client-")
    subprocess.run([*TEST, "--no-run"], cwd=WORKSPACE, env=fixture.environment, check=True)
    binary = fixture.go_build("go-server", "./cmd/graphite-meter")
    with go_server(fixture, binary, "open") as server:
        refused(fixture, report(server, *PATHS[0][1:]), False, UNTRUSTED, "untrusted")
        for label, protocol, throughput, latency in PATHS:
            result = subprocess.run(report(server, protocol, throughput, latency), env=environment(fixture, True),
                                    capture_output=True, text=True, timeout=90)
            record(result, fixture.directory / f"client-{protocol}-{throughput}.log", f"Rust client over {label}")
    with go_server(fixture, binary, "protected", protected=True) as server:
        refused(fixture, report(server, *PATHS[0][1:]), True, SIGN_IN, "headless-sign-in")
        sign_in(fixture, server)


if __name__ == "__main__":
    main()
