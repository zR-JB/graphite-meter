"""Run unchanged Go peers, the Go native client and process checks against the Rust server binary.

    python3 rust/interop/server.py

It builds the static musl server with the ci profile (needs musl-tools). Every server runs on this host's
runtime threads, so on Linux HTTP/3 is sharded over SO_REUSEPORT endpoints. Against a password-mode server the Go
peer signs in, then checks cookie-authenticated HTTP/3, one-use WebTransport and WebSocket tickets, and logout.
"""

from __future__ import annotations

import argparse
import re
import shutil
import signal
import subprocess
import time
from pathlib import Path

from fixture import ROOT, Fixture, Server, build_server, native, planned_endpoints, record, udp_sockets

PEER = ROOT / "rust/interop/peer.go"
LEGACY_RESET_OFFER = (
    "\t\tb = quicvarint.Append(b, uint64(legacyResetStreamAtParameterID))\n"
    "\t\tb = quicvarint.Append(b, 0)\n"
)
FEWER = "the buffer budget covers"
# Label, listener, throughput transport and latency transport of each Go native client run.
NATIVE = (
    ("HTTP/1.1", "http1", "fetch-stream", "websocket"),
    ("HTTPS HTTP/1.1", "http1-tls", "fetch-stream", "websocket"),
    ("HTTP/2", "http2", "fetch-stream", "websocket"),
    ("HTTP/3", "http3", "fetch-stream", "webtransport"),
    ("WebTransport", "http3", "webtransport", "webtransport"),
)


def allocator(binary: Path, fixture: Fixture) -> None:
    """The static build's mimalloc keeps huge pages off unless MIMALLOC_ALLOW_THP=2 restores them."""
    for setting, override in (("0", {}), ("2", {"MIMALLOC_ALLOW_THP": "2"})):
        environment = fixture.environment | {"MIMALLOC_VERBOSE": "1"} | override
        result = subprocess.run([str(binary), "--version"], env=environment, capture_output=True, text=True,
                                timeout=10, check=True)
        output = result.stdout + result.stderr
        (fixture.directory / f"mimalloc-allow-thp-{setting}.log").write_text(output)
        if not re.search(rf"option 'allow_thp': {setting}(?:\s|$)", output):
            raise RuntimeError(f"expected mimalloc allow_thp {setting}:\n{output}")
    print("mimalloc: allow_thp 0 by default, 2 with MIMALLOC_ALLOW_THP=2", flush=True)


def current_reset_peer(fixture: Fixture) -> Path:
    """The peer built against a disposable quic-go copy that offers only the current reliable-reset parameter."""
    module = subprocess.run(["go", "list", "-m", "-f", "{{.Dir}}", "github.com/quic-go/quic-go"], cwd=ROOT / "go",
                            env=fixture.environment, check=True, capture_output=True, text=True).stdout.strip()
    copy = fixture.directory / "quic-go-current-reset"
    shutil.copytree(module, copy)
    # Module-cache directories are read-only; the copy must be writable and removable.
    for path in [copy, *copy.rglob("*")]:
        path.chmod(path.stat().st_mode | (0o700 if path.is_dir() else 0o600))
    source = copy / "internal/wire/transport_parameters.go"
    original = source.read_text()
    if original.count(LEGACY_RESET_OFFER) != 1:
        raise RuntimeError("quic-go's reliable-reset offer changed; review the current-only peer")
    source.write_text(original.replace(LEGACY_RESET_OFFER, ""))
    modfile = fixture.directory / "current-reset.mod"
    modfile.write_bytes((ROOT / "go/go.mod").read_bytes())
    (fixture.directory / "current-reset.sum").write_bytes((ROOT / "go/go.sum").read_bytes())
    subprocess.run(["go", "mod", "edit", f"-modfile={modfile}", f"-replace=github.com/quic-go/quic-go={copy}"],
                   cwd=ROOT / "go", env=fixture.environment, check=True)
    return fixture.go_build("peer-current-reset", str(PEER), modfile=modfile)


def sharded(server: Server) -> None:
    """Every planned endpoint has its socket, and no line reports fewer."""
    planned = planned_endpoints()
    sockets = udp_sockets(server.h3)
    if sockets != planned or FEWER in server.output():
        raise RuntimeError(f"{sockets} HTTP/3 sockets, {planned} planned:\n{server.output()}")
    print(f"HTTP/3: {sockets} SO_REUSEPORT endpoints on {server.h3}", flush=True)


def peers(fixture: Fixture, server: Server, binaries: dict[str, Path], client: Path) -> None:
    for label, peer in binaries.items():
        result = subprocess.run([str(peer), "flow", str(server.h3)], cwd=fixture.directory, capture_output=True,
                                text=True, timeout=35)
        record(result, fixture.directory / f"peer-{label}.log", f"{label} Go peer")
    environment = fixture.environment | {"SSL_CERT_FILE": str(fixture.ca)}
    for label, listener, transport, latency in NATIVE:
        command = native(client, server, listener, transport, "latency,download,upload,bidirectional", 2,
                         "--latency-transport", latency)
        result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=90)
        record(result, fixture.directory / f"native-{listener}-{transport}.log", f"Go native client over {label}")


def fewer_endpoints(binary: Path, fixture: Fixture) -> None:
    """At the least budget the server accepts, one endpoint fits and a line says so."""
    planned = planned_endpoints()
    if planned < 2:
        print("HTTP/3: one runtime thread plans one endpoint; nothing can fall short", flush=True)
        return
    budget = 1
    for attempt in range(6):
        settings = {"GM_MAX_CONNECTIONS": "4", "GM_MAX_CONNECTIONS_PER_CLIENT": "4",
                    "GM_MAX_BUFFER_BYTES": str(budget)}
        server = fixture.server(binary, f"budget-{attempt}", settings)
        try:
            server.start()
        except RuntimeError:
            minimum = re.search(r"must be at least (\d+)", server.output())
            if minimum is None or int(minimum[1]) <= budget:
                raise
            budget = int(minimum[1])
            continue
        line = f"{FEWER} 1 of {planned} QUIC endpoints"
        sockets = udp_sockets(server.h3)
        server.stop()
        if line not in server.output() or sockets != 1:
            raise RuntimeError(f"expected {line!r} and one socket, found {sockets}:\n{server.output()}")
        print(f"HTTP/3 at the least budget, {budget} bytes: {line}", flush=True)
        return
    raise RuntimeError("the server refused every budget it named")


def shutdown_under_load(binary: Path, fixture: Fixture, client: Path) -> None:
    """SIGTERM during HTTP/3 and WebTransport downloads ends the server within its 5 s grace and 1 s close."""
    server = fixture.server(binary, "shutdown")
    server.start()
    environment = fixture.environment | {"SSL_CERT_FILE": str(fixture.ca)}
    loads = []
    for transport in ("fetch-stream", "webtransport"):
        log = (fixture.directory / f"load-{transport}.log").open("w")
        command = native(client, server, "http3", transport, "download", 30, "--streams=2", "--loaded-latency=false")
        loads.append(subprocess.Popen(command, env=environment, stdout=log, stderr=subprocess.STDOUT))
    try:
        deadline = time.monotonic() + 20
        while server.active() < 2:
            if time.monotonic() > deadline:
                raise TimeoutError(f"the downloads did not start:\n{server.output()}")
            time.sleep(0.1)
        time.sleep(2)
        elapsed = server.stop(signal.SIGTERM)
    finally:
        for load in loads:
            load.kill()
            load.wait()
    if elapsed > 6.5:
        raise RuntimeError(f"the server took {elapsed:.2f} s to exit after SIGTERM:\n{server.output()}")
    print(f"SIGTERM under HTTP/3 and WebTransport load: exited 0 after {elapsed:.2f} s", flush=True)


def main() -> None:
    argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter).parse_args()
    binary = build_server(ROOT, "ci")
    fixture = Fixture("interop-")
    allocator(binary, fixture)
    binaries = {"unchanged": fixture.go_build("peer", str(PEER)), "current-reset": current_reset_peer(fixture)}
    client = fixture.go_build("client", "./cmd/graphite-meter-client")
    with fixture.server(binary, "sharded") as server:
        sharded(server)
        peers(fixture, server, binaries, client)
    retry = {"GM_MAX_CONNECTIONS": "4", "GM_MAX_CONNECTIONS_PER_CLIENT": "4"}
    with fixture.server(binary, "retry", retry) as server:
        result = subprocess.run([str(binaries["unchanged"]), "retry", str(server.h3)], cwd=fixture.directory,
                                capture_output=True, text=True, timeout=35)
        record(result, fixture.directory / "peer-retry.log", "Go peer under connection pressure")
    protected = fixture.server(binary, "password")
    protected.protect()
    with protected as server:
        result = subprocess.run([str(binaries["unchanged"]), "auth", str(server.h3), str(server.h1_tls)],
                                cwd=fixture.directory, capture_output=True, text=True, timeout=35)
        record(result, fixture.directory / "peer-auth.log", "Go peer signed in with the password")
    fewer_endpoints(binary, fixture)
    shutdown_under_load(binary, fixture, client)


if __name__ == "__main__":
    main()
