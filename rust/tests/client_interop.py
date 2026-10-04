"""Run the native Rust measurement engine against the unchanged Go server."""

import socket
import ssl
import subprocess
import urllib.request

from process_fixture import Fixture, ROOT, password_auth, record, running, unused_ports

CARGO_TEST = ["cargo", "test", "--locked", "--workspace", "--profile", "ci", "--test", "go_server_interop"]


def probe(opener, url: str) -> bool:
    with opener.open(url, timeout=1) as response:
        return response.status == 200


def cargo_test(name: str, environment: dict[str, str], timeout: int = 120) -> subprocess.CompletedProcess:
    return subprocess.run([*CARGO_TEST, name, "--", "--exact", "--ignored", "--nocapture"], cwd=ROOT / "rust",
                          env=environment, capture_output=True, text=True, timeout=timeout)


def main() -> None:
    fixture = Fixture("client-interop-")
    directory = fixture.directory
    subprocess.run([*CARGO_TEST, "--no-run"], cwd=ROOT / "rust", check=True, timeout=600)
    ca, cert, key = fixture.identity()
    environment = fixture.go_environment()
    binary = directory / "go-server"
    subprocess.run(["go", "build", "-o", str(binary), "./cmd/graphite-meter"], cwd=ROOT / "go", env=environment,
                   check=True)
    h1_port, h2_port, h3_port = unused_ports(*[socket.SOCK_STREAM] * 3)
    discovery, h2_origin, h3_origin = (f"https://127.0.0.1:{port}" for port in (h1_port, h2_port, h3_port))
    context = ssl.create_default_context(cafile=str(ca))
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context))
    with running([
        str(binary), "--h1-addr=127.0.0.2:0", f"--h1-tls-addr=127.0.0.1:{h1_port}",
        f"--h2-addr=127.0.0.1:{h2_port}", f"--h3-addr=127.0.0.1:{h3_port}",
        f"--h1-tls-public-origin={discovery}", f"--h2-public-origin={h2_origin}", f"--h3-public-origin={h3_origin}",
        "--advertised-native-endpoints=http1-tls,http2,http3", f"--tls-cert={cert}", f"--tls-key={key}",
    ], environment, directory / "go-server.log", lambda: probe(opener, h3_origin + "/probe")):
        test = "go_server_completes_native_transport_stages"
        untrusted_env = {name: value for name, value in environment.items()
                         if name not in ("SSL_CERT_FILE", "SSL_CERT_DIR")}
        untrusted = cargo_test(test, untrusted_env | {"GM_GO_INTEROP_URL": discovery}, timeout=30)
        (directory / "rust-client-untrusted.log").write_text(untrusted.stdout + untrusted.stderr)
        if untrusted.returncode == 0 or "InvalidCertificate" not in untrusted.stderr:
            raise RuntimeError("Rust client did not reject the untrusted Go server certificate")
        print("Rust client rejected the untrusted Go server certificate", flush=True)
        trusted_env = untrusted_env | {"GM_GO_INTEROP_URL": discovery, "SSL_CERT_FILE": str(ca)}
        record(cargo_test(test, trusted_env), directory / "rust-client.log")

    auth_port, auth_h3_port = unused_ports(socket.SOCK_STREAM, socket.SOCK_STREAM)
    auth_url = f"https://127.0.0.1:{auth_port}"
    auth_environment = password_auth(environment, auth_url)
    with running([
        str(binary), "--h1-addr=127.0.0.2:0", f"--h1-tls-addr=127.0.0.1:{auth_port}", "--h2-addr=",
        f"--h3-addr=127.0.0.1:{auth_h3_port}", f"--h1-tls-public-origin={auth_url}",
        f"--h3-public-origin=https://127.0.0.1:{auth_h3_port}", "--advertised-native-endpoints=http1-tls,http3",
        f"--tls-cert={cert}", f"--tls-key={key}",
    ], auth_environment, directory / "go-auth-server.log", lambda: probe(opener, auth_url + "/login")):
        record(cargo_test("go_server_completes_approved_native_stages",
                          auth_environment | {"SSL_CERT_FILE": str(ca), "GM_GO_AUTH_URL": auth_url}),
               directory / "rust-client-auth.log")


if __name__ == "__main__":
    main()
