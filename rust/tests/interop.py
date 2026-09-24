"""Probe the Rust transport candidate against Graphite Meter's unchanged Go peer."""

import argparse
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]



def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, help="Prebuilt transport probe binary")
    args = parser.parse_args()
    binary = args.server
    if binary is None:
        subprocess.run(
            [
                "cargo", "build", "--locked",
                "-p", "graphite-meter-server",
                "--example", "h3_interop",
            ],
            cwd=ROOT / "rust",
            check=True,
        )
        binary = ROOT / "rust/target/debug/examples/h3_interop"
    binary = binary.resolve()
    with tempfile.TemporaryDirectory(prefix="gm-rust-interop-") as temporary:
        directory = Path(temporary)
        cert, key = directory / "cert.pem", directory / "key.pem"
        subprocess.run(
            [
                "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                "-days", "1",
                "-keyout", str(key),
                "-out", str(cert),
                "-subj", "/CN=localhost",
                "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost",
            ],
            check=True,
            capture_output=True,
        )
        client = directory / "client"
        subprocess.run(
            ["go", "build", "-o", str(client), str(ROOT / "rust/tests/h3_client.go")],
            cwd=ROOT / "go",
            check=True,
        )
        server = subprocess.Popen(
            [str(binary), "127.0.0.1:0", str(cert), str(key)],
            stdout=subprocess.PIPE,
            text=True,
        )
        try:
            assert server.stdout is not None
            with selectors.DefaultSelector() as ready:
                ready.register(server.stdout, selectors.EVENT_READ)
                if not ready.select(timeout=10):
                    raise TimeoutError("Rust listener did not report readiness")
            line = server.stdout.readline().strip()
            if not line.startswith("listening "):
                raise RuntimeError(f"Rust listener failed to start: {line}")
            address = line.removeprefix("listening ")
            # A nonzero exit is a failed compatibility gate. Never turn a
            # handshake or immediate-reset rejection into a pass.
            subprocess.run(
                [str(client), f"https://{address}", str(cert)],
                check=True,
                timeout=15,
            )
        finally:
            server.send_signal(signal.SIGINT)
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


if __name__ == "__main__":
    main()
