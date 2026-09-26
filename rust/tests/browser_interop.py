"""Run the existing Chromium functional suites against the actual Rust server.

Requires installed client dependencies, Bun with WebView support, Chrome, Rust,
and OpenSSL. Does not run throughput benchmarks or establish WAN performance.
"""

import argparse
import base64
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SUITES = {
    "transports": ["transports"],
    "connections": ["connections"],
    "authentication": ["authenticated-home", "authentication"],
    "measurement": ["partial-access", "multi-server"],
}
# Public test credential only: client/e2e/server-fleet.ts fixturePassword.
PASSWORD_HASH = (
    "$argon2id$v=19$m=19456,t=2,p=1$A7NMonU6E8f0wxduVIbV2Q$"
    "dG2EQgrjsIA+izAOngFgb0JLsJ1c6TyPgXAEWm86o2c"
)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", action="append", choices=SUITES)
    parser.add_argument("--skip-build", action="store_true", help="reuse a server with production browser assets embedded")
    parser.add_argument("--server", type=Path, default=ROOT / "rust/target/debug/graphite-meter-server")
    parser.add_argument("--port-base", type=int, default=19256)
    args = parser.parse_args()
    if not 1024 <= args.port_base <= 65400:
        parser.error("port base must leave room for the test fleets (1024..65400)")
    directory = Path(tempfile.mkdtemp(prefix="gm-rust-browser-"))
    print(f"Evidence: {directory}", flush=True)
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GM_")}
    chrome = os.environ.get("BUN_CHROME_PATH") or shutil.which("google-chrome") or shutil.which("chromium")
    if not chrome:
        parser.error("set BUN_CHROME_PATH to an installed Chrome executable")
    environment.update({
        "GM_CLIENT_BUILD_PROFILE": "prod",
        "GM_CLIENT_ALLOW_DUMMY": "0",
        "GM_RUST_ASSET_DIR": str(ROOT / "client/dist"),
        "BUN_CHROME_PATH": chrome,
        "TMPDIR": str(directory),
    })
    environment.pop("VERSION", None)

    def run(command: list[str], cwd: Path, name: str) -> None:
        print(f"Running {name}", flush=True)
        with (directory / f"{name}.log").open("w") as log:
            result = subprocess.run(command, cwd=cwd, env=environment, stdout=log, stderr=subprocess.STDOUT)
        print((directory / f"{name}.log").read_text()[-4000:], flush=True)
        result.check_returncode()

    if not args.skip_build:
        if args.server != ROOT / "rust/target/debug/graphite-meter-server":
            parser.error("use --skip-build with a custom --server")
        run(["bun", "run", "build:bundle"], ROOT / "client", "build-client")
        run(["cargo", "build", "--locked", "-p", "graphite-meter-server"], ROOT / "rust", "build-server")
    run(["bun", "run", "build:e2e-harness"], ROOT / "client", "build-harness")
    cert, key = directory / "cert.pem", directory / "key.pem"
    subprocess.run([
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        "-keyout", str(key), "-out", str(cert), "-subj", "/CN=127.0.0.1",
        "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost",
    ], check=True, capture_output=True)
    public = subprocess.check_output(["openssl", "x509", "-in", str(cert), "-pubkey", "-noout"])
    der = subprocess.run(["openssl", "pkey", "-pubin", "-outform", "der"], input=public, capture_output=True, check=True).stdout
    environment.update({
        "GM_E2E_SERVER_BIN": str(args.server.resolve()),
        "GM_E2E_TLS_CERT": str(cert),
        "GM_E2E_TLS_KEY": str(key),
        "GM_E2E_SPKI": base64.b64encode(hashlib.sha256(der).digest()).decode(),
        "GM_E2E_PORT_BASE": str(args.port_base),
        "GM_E2E_PASSWORD_HASH": PASSWORD_HASH,
        "GM_WEBVIEW_ARTIFACTS": str(directory / "artifacts"),
    })
    # Each file owns a fleet's afterAll cleanup; keep their Bun lifetimes separate.
    for suite in args.suite or ["transports", "connections", "authentication"]:
        for name in SUITES[suite]:
            run(["bun", "test", f"e2e/{name}.test.ts", "--no-orphans", "--timeout", "60000"], ROOT / "client", name)


if __name__ == "__main__":
    main()
