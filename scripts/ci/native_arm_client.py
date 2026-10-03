"""Temporary native ARM correctness smoke of the same-run cross-built release package."""
import os
from pathlib import Path
import re
import shutil
import socket
import sys
import tarfile
import urllib.request

from rust.tests.process_fixture import Fixture, ROOT, running, unused_port
from scripts.ci.rust_allocator_study import run

fixture = Fixture("native-arm-client-")
evidence = fixture.directory
try:
    assert os.uname().machine == "aarch64"
    base = "graphite-meter-client_0.0.0-ci_linux_arm64_rust"
    with tarfile.open(Path(sys.argv[1]) / f"{base}.tar.gz", "r:gz") as archive:
        member = archive.getmember(f"{base}/graphite-meter-client")
        assert member.isfile()
        client = evidence / "client"
        source = archive.extractfile(member)
        assert source is not None
        with source, client.open("wb") as destination:
            shutil.copyfileobj(source, destination)
    client.chmod(0o755)
    environment = fixture.go_environment() | {"GM_AUTH_MODE": "off"}
    for flag in ("-version", "-legal"):
        run([str(client), flag], environment, evidence / f"client{flag}.log", timeout=15)
    assert (evidence / "client-version.log").read_text().strip() == "graphite-meter-client 0.0.0-ci-rust"
    assert "UNREVIEWED DEVELOPMENT BUILD" not in (evidence / "client-legal.log").read_text()
    ca, cert, key = fixture.identity()
    environment["SSL_CERT_FILE"] = str(ca)
    environment.pop("SSL_CERT_DIR", None)
    ports = {protocol: unused_port(kind=socket.SOCK_DGRAM if protocol == "http3" else socket.SOCK_STREAM)
             for protocol in ("http1", "http2", "http3")}
    origins = {protocol: f'{"http" if protocol == "http1" else "https"}://127.0.0.1:{port}'
               for protocol, port in ports.items()}
    backend = evidence / "go-server"
    run(["go", "-C", str(ROOT / "go"), "build", "-o", str(backend), "./cmd/graphite-meter"],
        environment, evidence / "go-build.log", timeout=180)
    command = [str(backend), f'--h1-addr=127.0.0.1:{ports["http1"]}', "--h1-tls-addr=",
               f'--h2-addr=127.0.0.1:{ports["http2"]}', f'--h3-addr=127.0.0.1:{ports["http3"]}',
               "--advertised-native-endpoints=http1-clear,http2,http3", f"--tls-cert={cert}", f"--tls-key={key}",
               *(f"--h{protocol[-1]}-public-origin={origin}" for protocol, origin in origins.items())]
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with running(command, environment, evidence / "server.log",
                 lambda: opener.open(origins["http1"] + "/preflight", timeout=1).close() or True):
        for protocol, origin in origins.items():
            log = evidence / f"{protocol}.log"
            run([str(client), "-report", "-url", origins["http1"], "-streams", "4", "-stages", "bidirectional",
                 "-throughput-origin", origin, "-throughput-protocol", protocol, "-throughput-transport", "fetch-stream",
                 "-loaded-latency=false", "-warmup", "500ms", "-bidirectional-duration", "2s"],
                environment, log, timeout=30)
            report = log.read_text()
            assert "Complete" in report, report
            for arrow in ("↓", "↑"):
                assert re.search(rf"{arrow} Bidirectional\s+[1-9][0-9]*(?:\.[0-9]+)?\s+[kMGT]?bit/s", report), report
finally:
    for secret in ("ca.key", "key.pem"):
        (evidence / secret).unlink(missing_ok=True)
