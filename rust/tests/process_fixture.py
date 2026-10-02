"""Bounded startup and shutdown for real interoperability server processes."""
from contextlib import contextmanager
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[2]
PASSWORD_HASH = (
    "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$"
    "gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0"
)


def unused_port(kind: int = socket.SOCK_STREAM) -> int:
    with socket.socket(socket.AF_INET, kind) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class Fixture:
    """Retained evidence and isolated trust/build inputs for one real-peer run."""

    def __init__(self, prefix: str) -> None:
        target = ROOT / "rust/target"
        target.mkdir(exist_ok=True)
        self.directory = Path(tempfile.mkdtemp(prefix=prefix, dir=target))
        self.environment = {key: value for key, value in os.environ.items() if not key.startswith("GM_")}
        print(f"Evidence: {self.directory}", flush=True)

    def go_environment(self) -> dict[str, str]:
        cache, work = ROOT / "rust/target/interop-go-cache", self.directory / "go-tmp"
        cache.mkdir(exist_ok=True)
        work.mkdir()
        return self.environment | {"GOCACHE": str(cache), "GOTMPDIR": str(work)}

    def identity(self) -> tuple[Path, Path, Path]:
        """CA-signed P-256 loopback leaf, short enough for serverCertificateHashes."""
        directory = self.directory
        ca, cert, key = directory / "ca.pem", directory / "cert.pem", directory / "key.pem"
        ca_key, request, extensions = directory / "ca.key", directory / "server.csr", directory / "server.ext"
        ec = ["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes"]
        subprocess.run([
            "openssl", "req", "-x509", *ec, "-days", "10", "-subj", "/CN=Graphite Meter loopback test CA",
            "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign",
            "-keyout", str(ca_key), "-out", str(ca),
        ], check=True, capture_output=True)
        subprocess.run([
            "openssl", "req", *ec, "-subj", "/CN=localhost", "-keyout", str(key), "-out", str(request),
        ], check=True, capture_output=True)
        extensions.write_text(
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n"
            "extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1,IP:127.0.0.2,DNS:localhost\n"
        )
        subprocess.run([
            "openssl", "x509", "-req", "-in", str(request), "-CA", str(ca), "-CAkey", str(ca_key),
            "-set_serial", "1", "-days", "10", "-out", str(cert), "-extfile", str(extensions),
        ], check=True, capture_output=True)
        return ca, cert, key


@contextmanager
def running(command, environment, log, ready):
    with log.open("w") as output:
        process = subprocess.Popen(command, env=environment, stdout=output, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 10
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"server exited {process.returncode}: {log.read_text()}")
                try:
                    if result := ready():
                        break
                except (OSError, TimeoutError):
                    pass
                if time.monotonic() >= deadline:
                    raise TimeoutError(f"server did not become ready: {log.read_text()}")
                time.sleep(0.05)
            yield result
        finally:
            failed = sys.exc_info()[0] is not None
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                if not failed:
                    raise RuntimeError(f"server failed to shut down: {log.read_text()}")
            if process.returncode != 0 and not failed:
                raise RuntimeError(f"server exited {process.returncode}: {log.read_text()}")
