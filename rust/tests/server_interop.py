"""Run unchanged-Go-dependency peers against the assembled Rust server binary."""

import argparse
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
PASSWORD_HASH = (
    "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$"
    "gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0"
)


def unused_port(kind: int) -> int:
    with socket.socket(socket.AF_INET, kind) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=ROOT / "rust/target/debug/graphite-meter-server")
    args = parser.parse_args()
    directory = Path(tempfile.mkdtemp(prefix="server-interop-", dir=ROOT / "rust/target"))
    print(f"Evidence: {directory}", flush=True)
    cert, key = directory / "cert.pem", directory / "key.pem"
    subprocess.run([
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        "-keyout", str(key), "-out", str(cert), "-subj", "/CN=localhost",
        "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost",
    ], check=True, capture_output=True)
    client = directory / "client"
    subprocess.run(["go", "build", "-o", str(client), str(ROOT / "rust/tests/server_client.go")], cwd=ROOT / "go", check=True)
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GM_")}
    server_log = directory / "server.log"
    with server_log.open("w") as output:
        server = subprocess.Popen([
            str(args.server.resolve()), "--h1-addr=127.0.0.2:0", "--h1-tls-addr=", "--h2-addr=",
            "--h3-addr=127.0.0.1:0", f"--tls-cert={cert}", f"--tls-key={key}",
        ], env=environment, stdout=output, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 10
            address = None
            while time.monotonic() < deadline:
                for line in server_log.read_text().splitlines():
                    if line.startswith("graphite-meter Rust: http3 on "):
                        address = line.removeprefix("graphite-meter Rust: http3 on ")
                if address:
                    break
                if server.poll() is not None:
                    raise RuntimeError(server_log.read_text())
                time.sleep(0.05)
            if not address:
                raise TimeoutError("Rust H3 listener did not report readiness")
            result = subprocess.run([str(client), f"https://{address}", str(cert)], capture_output=True, text=True, timeout=35)
            (directory / "client.log").write_text(result.stdout + result.stderr)
            print(result.stdout + result.stderr, end="", flush=True)
            result.check_returncode()
            curl = shutil.which("curl")
            curl_version = (
                subprocess.run([curl, "--version"], check=True, capture_output=True, text=True).stdout
                if curl else ""
            )
            if "HTTP3" in curl_version:
                # curl reports a stream reset after the full byte count when an
                # empty request body is cancelled instead of observing its FIN.
                result = subprocess.run([
                    curl, "--http3-only", "--cacert", str(cert), "--fail", "--silent", "--show-error",
                    "--max-time", "10", "--output", os.devnull,
                    "--write-out", "%{http_code} %{http_version} %{size_download}",
                    f"https://{address}/download?bytes=65537",
                ], capture_output=True, text=True, timeout=12)
                (directory / "curl-h3.log").write_text(result.stdout + result.stderr)
                result.check_returncode()
                if result.stdout != "200 3 65537":
                    raise RuntimeError(f"curl HTTP/3 download mismatch: {result.stdout!r}")
                print("curl HTTP/3 download: clean FIN, 65537 bytes", flush=True)

                for method, path, allow in (
                    ("POST", "/download?bytes=65537", "GET, HEAD"),
                    ("GET", "/upload", "POST"),
                ):
                    headers = directory / f"curl-h3-{method.lower()}-{path.split('?')[0].strip('/')}.headers"
                    result = subprocess.run([
                        curl, "--http3-only", "--cacert", str(cert), "--silent", "--show-error",
                        "--max-time", "10", "--request", method, "--dump-header", str(headers),
                        "--output", os.devnull, "--write-out", "%{http_code} %{http_version}",
                        f"https://{address}{path}",
                    ], capture_output=True, text=True, timeout=12)
                    result.check_returncode()
                    response_headers = headers.read_text().lower()
                    if result.stdout != "405 3" or f"allow: {allow.lower()}" not in response_headers.splitlines():
                        raise RuntimeError(
                            f"curl HTTP/3 {method} {path}: {result.stdout!r}, {response_headers!r}"
                        )
                print("curl HTTP/3 method boundaries: 405 with Allow headers", flush=True)
        finally:
            server.send_signal(signal.SIGINT)
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
                raise RuntimeError("server failed to shut down within five seconds")
            if server.returncode != 0:
                raise RuntimeError(f"server exited {server.returncode}: {server_log.read_text()}")

    tls_port = unused_port(socket.SOCK_STREAM)
    h3_port = unused_port(socket.SOCK_DGRAM)
    while h3_port == tls_port:
        h3_port = unused_port(socket.SOCK_DGRAM)
    public = f"https://127.0.0.1:{tls_port}"
    auth_log = directory / "auth-server.log"
    auth_environment = {
        **environment,
        "GM_AUTH_MODE": "password",
        "GM_AUTH_PUBLIC_URL": public,
        "GM_AUTH_PASSWORD_HASH": PASSWORD_HASH,
    }
    with auth_log.open("w") as output:
        server = subprocess.Popen([
            str(args.server.resolve()), "--h1-addr=127.0.0.2:0",
            f"--h1-tls-addr=127.0.0.1:{tls_port}", "--h2-addr=",
            f"--h3-addr=127.0.0.1:{h3_port}",
            "--advertised-native-endpoints=http1-tls,http3",
            f"--tls-cert={cert}", f"--tls-key={key}",
        ], env=auth_environment, stdout=output, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if f"graphite-meter Rust: http3 on 127.0.0.1:{h3_port}" in auth_log.read_text():
                    break
                if server.poll() is not None:
                    raise RuntimeError(auth_log.read_text())
                time.sleep(0.05)
            else:
                raise TimeoutError("Authenticated Rust H3 listener did not report readiness")
            result = subprocess.run([
                str(client), f"https://127.0.0.1:{h3_port}", str(cert), public,
            ], capture_output=True, text=True, timeout=35)
            (directory / "auth-client.log").write_text(result.stdout + result.stderr)
            print(result.stdout + result.stderr, end="", flush=True)
            result.check_returncode()
        finally:
            server.send_signal(signal.SIGINT)
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
                raise RuntimeError("authenticated server failed to shut down within five seconds")
            if server.returncode != 0:
                raise RuntimeError(f"authenticated server exited {server.returncode}: {auth_log.read_text()}")


if __name__ == "__main__":
    main()
