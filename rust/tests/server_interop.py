"""Run unchanged-Go-dependency peers against the assembled Rust server binary."""

import argparse
import os
from pathlib import Path
import shutil
import socket
import subprocess

from process_fixture import Fixture, ROOT, password_auth, record, running, unused_ports

H3_TCP = "/tcp (HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe, upload and ticket control)"
# Build the binaries and both interop gates together: workspace dev-dependency features and the
# optimized ci profile match the later Cargo test and nextest invocations.
BUILD = ["cargo", "build", "--locked", "--workspace", "--profile", "ci", "--bins",
         "--test", "go_server_interop", "--test", "connection_faults"]


def build_current_reset_peer(directory: Path, environment: dict[str, str]) -> Path:
    """Build the pinned Go peer with only the current reliable-reset offer."""
    wire = subprocess.run(["go", "list", "-f", "{{.Dir}}", "github.com/quic-go/quic-go/internal/wire"],
                          cwd=ROOT / "go", env=environment, check=True, capture_output=True, text=True).stdout.strip()
    # Go forbids overlays of files in GOMODCACHE. Patch a disposable module
    # copy and point only this probe build at it through a temporary modfile.
    module = Path(wire).parents[1]
    local_module = directory / "quic-go-current-reset"
    shutil.copytree(module, local_module)
    # Module-cache directories are read-only; the disposable copy must be removable.
    for path in [local_module, *local_module.rglob("*")]:
        if path.is_dir():
            path.chmod(path.stat().st_mode | 0o700)
    source = local_module / "internal/wire/transport_parameters.go"
    legacy_offer = (
        "\t\tb = quicvarint.Append(b, uint64(legacyResetStreamAtParameterID))\n"
        "\t\tb = quicvarint.Append(b, 0)\n"
    )
    original = source.read_text()
    if original.count(legacy_offer) != 1:
        raise RuntimeError("quic-go reliable-reset offer changed; review the current-only probe")
    source.chmod(0o644)
    source.write_text(original.replace(legacy_offer, ""))
    modfile = directory / "probe.mod"
    modfile.write_bytes((ROOT / "go/go.mod").read_bytes())
    (directory / "probe.sum").write_bytes((ROOT / "go/go.sum").read_bytes())
    binary = directory / "client-current-reset"
    for command in (["mod", "edit", "-modfile", str(modfile), f"-replace=github.com/quic-go/quic-go={local_module}"],
                    ["build", "-modfile", str(modfile), "-o", str(binary), str(ROOT / "rust/tests/server_client.go")]):
        subprocess.run(["go", *command], cwd=ROOT / "go", env=environment, check=True)
    return binary


def h3_address(log: Path) -> str | None:
    return next((line.split(" listening on ", 1)[1].removesuffix(H3_TCP)
                 for line in log.read_text().splitlines()
                 if " listening on " in line and line.endswith(H3_TCP)), None)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, help="Prebuilt server binary")
    args = parser.parse_args()
    if args.server is None:
        subprocess.run(BUILD, cwd=ROOT / "rust", check=True)
        args.server = ROOT / "rust/target/ci/graphite-meter-server"
    fixture = Fixture("server-interop-")
    directory = fixture.directory
    ca, cert, key = fixture.identity()
    client = directory / "client"
    environment = fixture.go_environment()
    subprocess.run(["go", "build", "-o", str(client), str(ROOT / "rust/tests/server_client.go")],
                   cwd=ROOT / "go", env=environment, check=True)
    current_reset_client = build_current_reset_peer(directory, environment)
    server_log = directory / "server.log"
    with running([
        str(args.server.resolve()), "--h1-addr=127.0.0.2:0", "--h1-tls-addr=", "--h2-addr=",
        "--h3-addr=127.0.0.1:0", f"--tls-cert={cert}", f"--tls-key={key}",
    ], environment, server_log, lambda: h3_address(server_log)) as address:
        for label, peer in (("unchanged", client), ("current-reset", current_reset_client)):
            result = subprocess.run([str(peer), address.rpartition(":")[2]], cwd=directory,
                                    capture_output=True, text=True, timeout=35)
            record(result, directory / f"client-{label}.log", f"{label} Go peer:\n")
        curl = shutil.which("curl")
        if curl and "HTTP3" in subprocess.run([curl, "--version"], check=True, capture_output=True, text=True).stdout:
            # curl reports a stream reset after the full byte count when an
            # empty request body is cancelled instead of observing its FIN.
            result = subprocess.run([
                curl, "--http3-only", "--cacert", str(ca), "--fail", "--silent", "--show-error",
                "--max-time", "10", "--output", os.devnull,
                "--write-out", "%{http_code} %{http_version} %{size_download}",
                f"https://{address}/download?bytes=65537",
            ], capture_output=True, text=True, timeout=12)
            (directory / "curl-h3.log").write_text(result.stdout + result.stderr)
            result.check_returncode()
            if result.stdout != "200 3 65537":
                raise RuntimeError(f"curl HTTP/3 download mismatch: {result.stdout!r}")
            print("curl HTTP/3 download: clean FIN, 65537 bytes", flush=True)

    tls_port, h3_port = unused_ports(socket.SOCK_STREAM, socket.SOCK_DGRAM)
    public = f"https://127.0.0.1:{tls_port}"
    auth_log = directory / "auth-server.log"
    auth_environment = password_auth(environment, public)
    with running([
        str(args.server.resolve()), "--h1-addr=127.0.0.2:0", f"--h1-tls-addr=127.0.0.1:{tls_port}", "--h2-addr=",
        f"--h3-addr=127.0.0.1:{h3_port}", "--advertised-native-endpoints=http1-tls,http3",
        f"--tls-cert={cert}", f"--tls-key={key}",
    ], auth_environment, auth_log, lambda: f" listening on 127.0.0.1:{h3_port}{H3_TCP}" in auth_log.read_text()):
        record(subprocess.run([str(client), str(h3_port), str(tls_port)], cwd=directory,
                              capture_output=True, text=True, timeout=35), directory / "auth-client.log")
        native_environment = auth_environment | {"SSL_CERT_FILE": str(ca), "GM_RUST_INTEROP_URL": public}
        record(subprocess.run(["go", "test", "./internal/goclient", "-run", "^TestRustServerNativeApproval$",
                               "-count=1", "-v"], cwd=ROOT / "go", env=native_environment,
                              capture_output=True, text=True, timeout=80), directory / "native-client.log")


if __name__ == "__main__":
    main()
