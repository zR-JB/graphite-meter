"""Run the native Rust measurement engine against the unchanged Go server."""

import ssl
import subprocess
import urllib.request

from process_fixture import Fixture, PASSWORD_HASH, ROOT, running, unused_port


def probe(opener, url: str) -> bool:
    with opener.open(url, timeout=1) as response:
        return response.status == 200


def main() -> None:
    fixture = Fixture("client-interop-")
    directory = fixture.directory
    subprocess.run(
        ["cargo", "test", "--locked", "--workspace", "--profile", "ci",
         "--test", "go_server_interop", "--no-run"],
        cwd=ROOT / "rust", check=True, timeout=600,
    )
    ca, cert, key = fixture.identity()
    environment = fixture.go_environment()
    binary = directory / "go-server"
    subprocess.run(
        ["go", "build", "-o", str(binary), "./cmd/graphite-meter"],
        cwd=ROOT / "go",
        env=environment,
        check=True,
    )
    h1_port = unused_port()
    h2_port = unused_port()
    h3_port = unused_port()
    while len({h1_port, h2_port, h3_port}) != 3:
        h2_port, h3_port = unused_port(), unused_port()
    discovery = f"https://127.0.0.1:{h1_port}"
    h2_origin = f"https://127.0.0.1:{h2_port}"
    h3_origin = f"https://127.0.0.1:{h3_port}"
    log = directory / "go-server.log"
    context = ssl.create_default_context(cafile=str(ca))
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context))
    with running([
        str(binary), "--h1-addr=127.0.0.2:0",
        f"--h1-tls-addr=127.0.0.1:{h1_port}",
        f"--h2-addr=127.0.0.1:{h2_port}",
        f"--h3-addr=127.0.0.1:{h3_port}",
        f"--h1-tls-public-origin={discovery}",
        f"--h2-public-origin={h2_origin}",
        f"--h3-public-origin={h3_origin}",
        "--advertised-native-endpoints=http1-tls,http2,http3",
        f"--tls-cert={cert}", f"--tls-key={key}",
    ], environment, log,
        lambda: probe(opener, h3_origin + "/probe")):
        command = [
            "cargo", "test", "--locked", "--workspace", "--profile", "ci",
            "--test", "go_server_interop", "go_server_completes_native_transport_stages",
            "--", "--exact", "--ignored", "--nocapture",
        ]
        untrusted_env = {**environment, "GM_GO_INTEROP_URL": discovery}
        untrusted_env.pop("SSL_CERT_FILE", None)
        untrusted_env.pop("SSL_CERT_DIR", None)
        untrusted = subprocess.run(
            command,
            cwd=ROOT / "rust",
            env=untrusted_env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        (directory / "rust-client-untrusted.log").write_text(
            untrusted.stdout + untrusted.stderr
        )
        if untrusted.returncode == 0 or "InvalidCertificate" not in untrusted.stderr:
            raise RuntimeError("Rust client did not reject the untrusted Go server certificate")
        print("Rust client rejected the untrusted Go server certificate", flush=True)
        trusted_env = {**environment, "GM_GO_INTEROP_URL": discovery, "SSL_CERT_FILE": str(ca)}
        trusted_env.pop("SSL_CERT_DIR", None)
        native = subprocess.run(
            command,
            cwd=ROOT / "rust",
            env=trusted_env,
            capture_output=True,
            text=True,
            timeout=120,
        )
        (directory / "rust-client.log").write_text(native.stdout + native.stderr)
        print(native.stdout + native.stderr, end="", flush=True)
        native.check_returncode()

    auth_port = unused_port()
    auth_h3_port = unused_port()
    while auth_h3_port == auth_port:
        auth_h3_port = unused_port()
    auth_url = f"https://127.0.0.1:{auth_port}"
    auth_log = directory / "go-auth-server.log"
    auth_environment = {
        **environment,
        "GM_AUTH_MODE": "password",
        "GM_AUTH_PUBLIC_URL": auth_url,
        "GM_AUTH_PASSWORD_HASH": PASSWORD_HASH,
    }
    with running([
        str(binary), "--h1-addr=127.0.0.2:0",
        f"--h1-tls-addr=127.0.0.1:{auth_port}",
        "--h2-addr=",
        f"--h3-addr=127.0.0.1:{auth_h3_port}",
        f"--h1-tls-public-origin={auth_url}",
        f"--h3-public-origin=https://127.0.0.1:{auth_h3_port}",
        "--advertised-native-endpoints=http1-tls,http3",
        f"--tls-cert={cert}", f"--tls-key={key}",
    ], auth_environment, auth_log,
        lambda: probe(opener, auth_url + "/login")):
        native = subprocess.run(
            [
                "cargo", "test", "--locked", "--workspace", "--profile", "ci",
                "--test", "go_server_interop", "go_server_completes_approved_native_stages",
                "--", "--exact", "--ignored", "--nocapture",
            ],
            cwd=ROOT / "rust",
            env={
                **auth_environment,
                "SSL_CERT_FILE": str(ca),
                "GM_GO_AUTH_URL": auth_url,
            },
            capture_output=True,
            text=True,
            timeout=120,
        )
        (directory / "rust-client-auth.log").write_text(native.stdout + native.stderr)
        print(native.stdout + native.stderr, end="", flush=True)
        native.check_returncode()


if __name__ == "__main__":
    main()
