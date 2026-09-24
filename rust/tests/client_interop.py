"""Run the native Rust measurement engine against the unchanged Go server."""

import os
from pathlib import Path
import signal
import socket
import ssl
import subprocess
import tempfile
import time
import urllib.request

from server_interop import PASSWORD_HASH


ROOT = Path(__file__).resolve().parents[2]


def unused_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def main() -> None:
    target = ROOT / "rust/target"
    target.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="client-interop-", dir=target))
    print(f"Evidence: {directory}", flush=True)
    cert, key = directory / "cert.pem", directory / "key.pem"
    ca, ca_key, request = directory / "ca.pem", directory / "ca.key", directory / "server.csr"
    subprocess.run(
        [
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-days", "1", "-keyout", str(ca_key), "-out", str(ca),
            "-subj", "/CN=Graphite Meter loopback test CA",
            "-addext", "basicConstraints=critical,CA:TRUE",
            "-addext", "keyUsage=critical,keyCertSign,cRLSign",
        ],
        check=True,
        capture_output=True,
    )
    subprocess.run(
        [
            "openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
            "-keyout", str(key), "-out", str(request), "-subj", "/CN=localhost",
        ],
        check=True,
        capture_output=True,
    )
    extensions = directory / "server.ext"
    extensions.write_text(
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\n"
        "subjectAltName=IP:127.0.0.1,DNS:localhost\n"
    )
    subprocess.run(
        [
            "openssl", "x509", "-req", "-in", str(request),
            "-CA", str(ca), "-CAkey", str(ca_key), "-set_serial", "1",
            "-days", "1", "-out", str(cert), "-extfile", str(extensions),
        ],
        check=True,
        capture_output=True,
    )
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GM_")}
    cache = target / "interop-go-cache"
    cache.mkdir(exist_ok=True)
    work = directory / "go-tmp"
    work.mkdir()
    environment.update({"GOCACHE": str(cache), "GOTMPDIR": str(work)})
    binary = directory / "go-server"
    subprocess.run(
        ["go", "build", "-o", str(binary), "./cmd/graphite-meter"],
        cwd=ROOT / "go",
        env=environment,
        check=True,
    )
    h1_port = unused_port()
    h2_port = unused_port()
    h3_port = unused_port()
    while len({h1_port, h2_port, h3_port}) != 3:
        h2_port, h3_port = unused_port(), unused_port()
    discovery = f"https://127.0.0.1:{h1_port}"
    h2_origin = f"https://127.0.0.1:{h2_port}"
    h3_origin = f"https://127.0.0.1:{h3_port}"
    log = directory / "go-server.log"
    with log.open("w") as output:
        server = subprocess.Popen(
            [
                str(binary), "--h1-addr=127.0.0.2:0",
                f"--h1-tls-addr=127.0.0.1:{h1_port}",
                f"--h2-addr=127.0.0.1:{h2_port}",
                f"--h3-addr=127.0.0.1:{h3_port}",
                f"--h1-tls-public-origin={discovery}",
                f"--h2-public-origin={h2_origin}",
                f"--h3-public-origin={h3_origin}",
                "--advertised-native-endpoints=http1-tls,http2,http3",
                f"--tls-cert={cert}", f"--tls-key={key}",
            ],
            env=environment,
            stdout=output,
            stderr=subprocess.STDOUT,
        )
        try:
            context = ssl.create_default_context(cafile=str(ca))
            opener = urllib.request.build_opener(
                urllib.request.ProxyHandler({}),
                urllib.request.HTTPSHandler(context=context),
            )
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if server.poll() is not None:
                    raise RuntimeError(f"Go server exited: {log.read_text()}")
                try:
                    with opener.open(h3_origin + "/probe", timeout=1) as response:
                        if response.status == 200:
                            break
                except (OSError, TimeoutError):
                    time.sleep(0.05)
            else:
                raise TimeoutError(f"Go HTTP/3 bootstrap did not start: {log.read_text()}")
            command = [
                "cargo", "test", "--locked", "-p", "graphite-meter-client",
                "--test", "go_server_interop", "--", "--nocapture",
            ]
            untrusted_env = {**environment, "GM_GO_INTEROP_URL": discovery}
            untrusted_env.pop("SSL_CERT_FILE", None)
            untrusted_env.pop("SSL_CERT_DIR", None)
            untrusted = subprocess.run(
                command,
                cwd=ROOT / "rust",
                env=untrusted_env,
                capture_output=True,
                text=True,
                timeout=30,
            )
            (directory / "rust-client-untrusted.log").write_text(
                untrusted.stdout + untrusted.stderr
            )
            if untrusted.returncode == 0 or "InvalidCertificate" not in untrusted.stderr:
                raise RuntimeError("Rust client did not reject the untrusted Go server certificate")
            print("Rust client rejected the untrusted Go server certificate", flush=True)
            trusted_env = {**environment, "GM_GO_INTEROP_URL": discovery, "SSL_CERT_FILE": str(ca)}
            trusted_env.pop("SSL_CERT_DIR", None)
            native = subprocess.run(
                command,
                cwd=ROOT / "rust",
                env=trusted_env,
                capture_output=True,
                text=True,
                timeout=120,
            )
            (directory / "rust-client.log").write_text(native.stdout + native.stderr)
            print(native.stdout + native.stderr, end="", flush=True)
            native.check_returncode()
        finally:
            server.send_signal(signal.SIGINT)
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
                raise RuntimeError("Go server failed to shut down within five seconds")
            if server.returncode != 0:
                raise RuntimeError(f"Go server exited {server.returncode}: {log.read_text()}")

    auth_port = unused_port()
    auth_h3_port = unused_port()
    while auth_h3_port == auth_port:
        auth_h3_port = unused_port()
    auth_url = f"https://127.0.0.1:{auth_port}"
    auth_log = directory / "go-auth-server.log"
    auth_environment = {
        **environment,
        "GM_AUTH_MODE": "password",
        "GM_AUTH_PUBLIC_URL": auth_url,
        "GM_AUTH_PASSWORD_HASH": PASSWORD_HASH,
    }
    with auth_log.open("w") as output:
        server = subprocess.Popen(
            [
                str(binary), "--h1-addr=127.0.0.2:0",
                f"--h1-tls-addr=127.0.0.1:{auth_port}",
                "--h2-addr=",
                f"--h3-addr=127.0.0.1:{auth_h3_port}",
                f"--h1-tls-public-origin={auth_url}",
                f"--h3-public-origin=https://127.0.0.1:{auth_h3_port}",
                "--advertised-native-endpoints=http1-tls,http3",
                f"--tls-cert={cert}", f"--tls-key={key}",
            ],
            env=auth_environment,
            stdout=output,
            stderr=subprocess.STDOUT,
        )
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if server.poll() is not None:
                    raise RuntimeError(f"authenticated Go server exited: {auth_log.read_text()}")
                try:
                    with opener.open(auth_url + "/login", timeout=1) as response:
                        if response.status == 200:
                            break
                except (OSError, TimeoutError):
                    time.sleep(0.05)
            else:
                raise TimeoutError(f"authenticated Go server did not start: {auth_log.read_text()}")
            native = subprocess.run(
                [
                    "cargo", "test", "--locked", "-p", "graphite-meter-client",
                    "--test", "go_server_interop", "go_server_completes_approved_native_stages",
                    "--", "--nocapture",
                ],
                cwd=ROOT / "rust",
                env={
                    **auth_environment,
                    "SSL_CERT_FILE": str(ca),
                    "GM_GO_AUTH_URL": auth_url,
                },
                capture_output=True,
                text=True,
                timeout=120,
            )
            (directory / "rust-client-auth.log").write_text(native.stdout + native.stderr)
            print(native.stdout + native.stderr, end="", flush=True)
            native.check_returncode()
        finally:
            server.send_signal(signal.SIGINT)
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
                raise RuntimeError("authenticated Go server failed to shut down")
            if server.returncode != 0:
                raise RuntimeError(f"authenticated Go server exited {server.returncode}: {auth_log.read_text()}")


if __name__ == "__main__":
    main()
