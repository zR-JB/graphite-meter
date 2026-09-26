"""Probe Rust HTTP/3 against unchanged and current-codepoint-only Go peers."""

import argparse
import selectors
import shutil
import signal
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def build_current_reset_peer(directory: Path) -> Path:
    """Build the pinned Go peer with only the current reliable-reset offer."""
    wire = subprocess.run(
        ["go", "list", "-f", "{{.Dir}}", "github.com/quic-go/quic-go/internal/wire"],
        cwd=ROOT / "go",
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    # Go forbids overlays of files in GOMODCACHE. Patch a disposable module
    # copy and point only this probe build at it through a temporary modfile.
    module = Path(wire).parents[1]
    local_module = directory / "quic-go-current-reset"
    shutil.copytree(module, local_module)
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
    subprocess.run(
        [
            "go",
            "mod",
            "edit",
            "-modfile",
            str(modfile),
            f"-replace=github.com/quic-go/quic-go={local_module}",
        ],
        cwd=ROOT / "go",
        check=True,
    )
    binary = directory / "client-current-reset"
    subprocess.run(
        [
            "go",
            "build",
            "-modfile",
            str(modfile),
            "-o",
            str(binary),
            str(ROOT / "rust/tests/h3_client.go"),
        ],
        cwd=ROOT / "go",
        check=True,
    )
    return binary


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
        current_reset_client = build_current_reset_peer(directory)
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
            for label, peer in [
                ("unchanged Go peer", client),
                ("current-only reliable-reset Go peer", current_reset_client),
            ]:
                print(f"Probing {label}", flush=True)
                subprocess.run(
                    [str(peer), address.rpartition(":")[2]],
                    cwd=directory,
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
