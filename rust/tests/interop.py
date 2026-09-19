"""Probe the Rust transport candidate against Graphite Meter's Go peer."""

import argparse
import json
from pathlib import Path
import selectors
import shutil
import signal
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def reset_reader_overlay(directory: Path) -> list[str]:
    """Test-only adapter correction; never changes module sources or product builds."""
    module = json.loads(
        subprocess.check_output(
            ["go", "list", "-m", "-json", "github.com/quic-go/quic-go"],
            cwd=ROOT / "go",
            text=True,
        )
    )
    if module["Version"] != "v0.62.0" or "Replace" in module:
        raise RuntimeError("Reset-reader overlay requires unreplaced quic-go v0.62.0")
    source = Path(module["Dir"]) / "quicvarint/io.go"
    original = source.read_text()
    expected = """func (r *byteReader) ReadByte() (byte, error) {
\tvar b [1]byte
\tvar n int
\tvar err error
\tfor n == 0 && err == nil {
\t\tn, err = r.Read(b[:])
\t}

\tif n == 1 && err == io.EOF {
\t\terr = nil
\t}
\treturn b[0], err
}"""
    replacement = """func (r *byteReader) ReadByte() (byte, error) {
\tvar b [1]byte
\t_, err := io.ReadFull(r.Reader, b[:])
\treturn b[0], err
}"""
    if original.count(expected) != 1:
        raise RuntimeError("quic-go ReadByte source changed; refusing test overlay")
    # Go forbids overlays inside GOMODCACHE. Copy the module unchanged, then
    # redirect only this diagnostic build through a temporary modfile.
    module_copy = directory / "quic-go"
    shutil.copytree(Path(module["Dir"]), module_copy)
    source = module_copy / "quicvarint/io.go"
    modfile = directory / "probe.mod"
    replacement_directive = (
        f"\nreplace github.com/quic-go/quic-go => {json.dumps(str(module_copy))}\n"
    )
    modfile.write_text((ROOT / "go/go.mod").read_text() + replacement_directive)
    shutil.copyfile(ROOT / "go/go.sum", directory / "probe.sum")
    corrected = directory / "quicvarint_io.go"
    corrected.write_text(original.replace(expected, replacement))
    overlay = directory / "go-overlay.json"
    overlay.write_text(json.dumps({"Replace": {str(source): str(corrected)}}))
    print(
        "TEST-ONLY: quic-go v0.62.0 ReadByte correction enabled; "
        "this is not the unchanged-peer gate",
        flush=True,
    )
    return ["-modfile", str(modfile), "-overlay", str(overlay)]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, help="Prebuilt transport probe binary")
    parser.add_argument(
        "--fix-go-reset-reader",
        action="store_true",
        help="Test-only quic-go v0.62.0 ReadByte overlay; does not establish unchanged-peer parity",
    )
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
        build = ["go", "build"]
        if args.fix_go_reset_reader:
            build += reset_reader_overlay(directory)
        subprocess.run(
            [*build, "-o", str(client), str(ROOT / "rust/tests/h3_client.go")],
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
            # A nonzero exit is a failed compatibility gate, including a known
            # upstream limitation. Never turn a handshake rejection into a pass.
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
