"""Builds, identities and bounded server processes for runs against the Rust server or with the Rust client."""

from __future__ import annotations

import json
import os
import platform
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from types import TracebackType

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/ci"))
import rust_workspace  # noqa: E402

# Image platforms by the machine names Python reports.
PLATFORMS = {"x86_64": "linux/amd64", "aarch64": "linux/arm64"}
PASSWORD = "correct horse battery staple"
PASSWORD_HASH = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0"


def musl_target() -> str:
    """The shipped server target for this machine."""
    return rust_workspace.load().server[PLATFORMS[platform.machine()]]


def build_server(profile: str) -> Path:
    """Builds the static musl server with `profile`, without notices or browser assets."""
    target = musl_target()
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("GM_", "CARGO_PROFILE_")) and not key.endswith("RUSTFLAGS")}
    environment["CC_" + target.replace("-", "_")] = f"{platform.machine()}-linux-musl-gcc"
    workspace = ROOT / "rust"
    subprocess.run(["rustup", "target", "add", target], cwd=workspace, check=True)
    subprocess.run(["cargo", "build", "--locked", "--profile", profile, "--target", target,
                    "-p", "graphite-meter-server"], cwd=workspace, env=environment, check=True)
    return workspace / "target" / target / profile / "graphite-meter-server"


def planned_endpoints() -> int:
    """The HTTP/3 endpoints a server plans on this host: half its runtime threads, two to sixteen."""
    workers = int(os.environ.get("TOKIO_WORKER_THREADS") or len(os.sched_getaffinity(0)))
    return min(max(workers // 2, 2), 16) if workers > 1 else 1


def free_port() -> int:
    """A loopback port free for both TCP and UDP, as HTTP/3 and its companion share one."""
    while True:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
            udp.bind(("127.0.0.1", 0))
            port = udp.getsockname()[1]
            with socket.socket() as tcp:
                try:
                    tcp.bind(("127.0.0.1", port))
                except OSError:
                    continue
                return port


def udp_sockets(port: int) -> int:
    """The UDP sockets bound to 127.0.0.1:`port`; SO_REUSEPORT endpoints each have one."""
    local = f"0100007F:{port:04X}"
    lines = Path("/proc/net/udp").read_text().splitlines()[1:]
    return sum(1 for line in lines if line.split()[1] == local)


def native(client: Path, server: Server, listener: str, transport: str, stages: str, seconds: int,
           *extra: str) -> list[str]:
    """A native client's report run against `server`'s `listener`."""
    protocol = "http1" if listener.startswith("http1") else listener
    durations = [f"--{stage}-duration={seconds}s" for stage in ("latency", "download", "upload", "bidirectional")]
    return [str(client), "--report", "--url", server.origin("http1"), "--throughput-origin", server.origin(listener),
            "--throughput-protocol", protocol, "--throughput-transport", transport, "--stages", stages,
            "--warmup=250ms", *durations, *extra]


def record(result: subprocess.CompletedProcess[str], log: Path, label: str) -> None:
    """Keeps and prints a finished peer's output, then fails if the peer failed."""
    log.write_text(result.stdout + result.stderr)
    print(f"{label}:\n{result.stdout}{result.stderr}", end="", flush=True)
    result.check_returncode()


class Fixture:
    """Evidence directory and trust material for one run."""

    def __init__(self, prefix: str) -> None:
        # TMPDIR; the CI jobs set it to the runner's job directory, which no cache restores.
        self.directory = Path(tempfile.mkdtemp(prefix=prefix))
        self.environment = {key: value for key, value in os.environ.items()
                            if not key.startswith(("GM_", "MIMALLOC_"))}
        self.ca, self.cert, self.key = self.identity()
        print(f"Evidence: {self.directory}", flush=True)

    def go_build(self, output: str, *arguments: str, modfile: Path | None = None) -> Path:
        """Builds a Go program against go/go.mod, or `modfile`, into the evidence directory."""
        binary = self.directory / output
        flags = [f"-modfile={modfile}"] if modfile else []
        subprocess.run(["go", "build", *flags, "-trimpath", "-o", str(binary), *arguments], cwd=ROOT / "go",
                       env=self.environment, check=True)
        return binary

    def identity(self) -> tuple[Path, Path, Path]:
        """A CA and its P-256 loopback leaf, short-lived enough for serverCertificateHashes."""
        directory = self.directory
        ca, cert, key = directory / "ca.pem", directory / "cert.pem", directory / "key.pem"
        ca_key, request, extensions = directory / "ca.key", directory / "server.csr", directory / "server.ext"
        ec = ["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes"]
        extensions.write_text(
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n"
            "extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1,DNS:localhost\n")
        for command in (
            ["req", "-x509", *ec, "-days", "10", "-subj", "/CN=Graphite Meter loopback test CA",
             "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign",
             "-keyout", ca_key, "-out", ca],
            ["req", *ec, "-subj", "/CN=localhost", "-keyout", key, "-out", request],
            ["x509", "-req", "-in", request, "-CA", ca, "-CAkey", ca_key, "-set_serial", "1", "-days", "10",
             "-out", cert, "-extfile", extensions],
        ):
            subprocess.run(["openssl", *map(str, command)], check=True, capture_output=True)
        return ca, cert, key

    def server(self, binary: Path, name: str, settings: dict[str, str] | None = None) -> Server:
        return Server(self, binary, name, settings or {})


class Server:
    """A server process on fresh loopback ports serving every listener until the context ends."""

    def __init__(self, fixture: Fixture, binary: Path, name: str, settings: dict[str, str]) -> None:
        self.binary, self.log = binary, fixture.directory / f"server-{name}.log"
        ports: list[int] = []
        while len(set(ports)) != 4:
            ports = [free_port() for _ in range(4)]
        self.h1, self.h1_tls, self.h2, self.h3 = ports
        self.environment = fixture.environment | {
            "GM_H1_ADDR": f"127.0.0.1:{self.h1}", "GM_H1_TLS_ADDR": f"127.0.0.1:{self.h1_tls}",
            "GM_H2_ADDR": f"127.0.0.1:{self.h2}", "GM_H3_ADDR": f"127.0.0.1:{self.h3}",
            "GM_TLS_CERT": str(fixture.cert), "GM_TLS_KEY": str(fixture.key),
        } | settings
        self.process: subprocess.Popen[bytes] | None = None

    def origin(self, listener: str) -> str:
        return {"http1": f"http://127.0.0.1:{self.h1}", "http1-tls": f"https://127.0.0.1:{self.h1_tls}",
                "http2": f"https://127.0.0.1:{self.h2}", "http3": f"https://127.0.0.1:{self.h3}"}[listener]

    def output(self) -> str:
        return self.log.read_text()

    def protect(self) -> None:
        """Password mode, signed in at the HTTPS HTTP/1.1 origin, advertising only the TLS listeners."""
        self.environment |= {"GM_AUTH_MODE": "password", "GM_AUTH_PUBLIC_URL": self.origin("http1-tls"),
                             "GM_AUTH_PASSWORD_HASH": PASSWORD_HASH,
                             "GM_ADVERTISED_NATIVE_ENDPOINTS": "http1-tls,http2,http3"}

    def start(self) -> subprocess.Popen[bytes]:
        """Starts the server; it is ready once its clear listener answers."""
        with self.log.open("w") as log:
            self.process = subprocess.Popen([str(self.binary)], env=self.environment, stdout=log,
                                            stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 15
        while True:
            if self.process.poll() is not None:
                raise RuntimeError(f"server exited {self.process.returncode}:\n{self.output()}")
            try:
                self.probe()
                return self.process
            except urllib.error.HTTPError:
                return self.process
            except OSError:
                if time.monotonic() >= deadline:
                    raise TimeoutError(f"server did not become ready:\n{self.output()}") from None
                time.sleep(0.05)

    def probe(self) -> dict[str, object]:
        with urllib.request.urlopen(f"{self.origin('http1')}/probe", timeout=1) as answer:
            return json.load(answer)

    def active(self) -> int:
        load = self.probe()["load"]
        assert isinstance(load, dict)
        return int(load["active"])

    def stop(self, sig: signal.Signals = signal.SIGINT, bound: float = 10) -> float:
        """Signals the server and returns how long it took to exit; it must exit 0 within `bound` seconds."""
        assert self.process is not None
        started = time.monotonic()
        self.process.send_signal(sig)
        try:
            status = self.process.wait(timeout=bound)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            raise RuntimeError(f"server did not exit within {bound} s:\n{self.output()}") from None
        if status != 0:
            raise RuntimeError(f"server exited {status}:\n{self.output()}")
        return time.monotonic() - started

    def __enter__(self) -> Server:
        self.start()
        return self

    def __exit__(self, kind: type[BaseException] | None, error: BaseException | None,
                 trace: TracebackType | None) -> None:
        if self.process is None:
            return
        if kind is not None:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
        elif self.process.poll() is None:
            self.stop()
        elif self.process.returncode != 0:
            raise RuntimeError(f"server exited {self.process.returncode}:\n{self.output()}")
