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
    subprocess.run(
        [
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-days", "1", "-keyout", str(key), "-out", str(cert),
            "-subj", "/CN=localhost",
            "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost",
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
    h3_port = unused_port()
    while h3_port == h1_port:
        h3_port = unused_port()
    discovery = f"http://127.0.0.1:{h1_port}"
    h3_origin = f"https://127.0.0.1:{h3_port}"
    log = directory / "go-server.log"
    with log.open("w") as output:
        server = subprocess.Popen(
            [
                str(binary), f"--h1-addr=127.0.0.1:{h1_port}",
                "--h1-tls-addr=", "--h2-addr=",
                f"--h3-addr=127.0.0.1:{h3_port}",
                f"--h1-public-origin={discovery}",
                f"--h3-public-origin={h3_origin}",
                "--advertised-native-endpoints=http1-clear,http3",
                f"--tls-cert={cert}", f"--tls-key={key}",
            ],
            env=environment,
            stdout=output,
            stderr=subprocess.STDOUT,
        )
        try:
            context = ssl.create_default_context(cafile=str(cert))
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
            native = subprocess.run(
                [
                    "cargo", "test", "--locked", "-p", "graphite-meter-client",
                    "--test", "go_server_interop", "--", "--nocapture",
                ],
                cwd=ROOT / "rust",
                env={**environment, "GM_GO_INTEROP_URL": discovery},
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


if __name__ == "__main__":
    main()
