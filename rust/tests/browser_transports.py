"""Drive Chromium, Firefox and WebKitGTK against a server's HTTP/3 and WebTransport routes.

    python3 rust/tests/browser_transports.py --server BINARY

BINARY is a Rust or Go server; both take the shared GM_* flags. The browsers need fixed loopback
ports 17246-17248, so run this in a private network namespace with loopback up. Chromium and
Firefox must use HTTP/3 and WebTransport and see a server-ended session's close code and reason.
WebKitGTK has neither, so it is the negative control: no WebTransport, fetch over HTTP/2.
"""

import argparse
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import ssl
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
PAGE, H3, H2 = 17246, 17247, 17248
CHROMIUM = "/usr/bin/chromium"
FIREFOX = str(Path.home() / ".cache/ms-playwright/firefox-1538/firefox/firefox")
MINIBROWSER = "/usr/lib/webkitgtk-6.0/MiniBrowser"
MIB = 1024 * 1024
TRANSPORT_CHECKS = {
    "download": lambda d: d == {"bytes": MIB, "protocol": "h3"},
    "upload": lambda d: d == {"bytes": MIB, "protocol": "h3"},
    "wt_ping": lambda d: isinstance(d, str) and d.startswith("PONG,7,"),
    "wt_download": lambda d: d == 65536,
    "wt_upload": lambda d: isinstance(d, int) and d >= 65536,
    "wt_close": lambda d: d == {"closeCode": 2, "reason": "lifetime"},
}
NEGATIVE_CHECKS = {
    "webtransport": lambda d: d == "undefined",
    "download": lambda d: d == {"bytes": 65536, "protocol": "h2"},
}
# No service directories: the default session bus activates portals, gvfs and the keyring.
BUS = """<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/>
</policy></busconfig>"""
PAGE_HTML = """<!doctype html><meta charset="utf-8"><title>transports</title><script>
const query = new URLSearchParams(location.search);
const base = query.get("base");
const hash = query.get("hash");
const options = hash ? { serverCertificateHashes: [{ algorithm: "sha-256", value: Uint8Array.from(hash.split(","), Number) }] } : {};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const within = (promise, ms) => Promise.race([promise, sleep(ms).then(() => { throw new Error(`timeout ${ms} ms`); })]);
const protocol = (url) => performance.getEntriesByName(url).at(-1)?.nextHopProtocol;
async function drain(readable) {
  const reader = readable.getReader();
  for (let bytes = 0; ; ) {
    const { done, value } = await reader.read();
    if (done) return bytes;
    bytes += value.byteLength;
  }
}
async function fetched(url, init) {
  const response = await fetch(url, { cache: "no-store", ...init });
  if (!response.ok) throw new Error(`status ${response.status}`);
  return response;
}
async function uploadId() {
  return (await (await fetched(base + "/upload/session", { method: "POST" })).json()).uploadId;
}
async function session(path, run) {
  const wt = new WebTransport(base + path, options);
  await within(wt.ready, 5000);
  try { return await run(wt); } finally { wt.close(); await wt.closed.catch(() => {}); }
}
async function* records(readable) {
  const reader = readable.pipeThrough(new TextDecoderStream()).getReader();
  for (let buffered = ""; ; ) {
    const { done, value } = await reader.read();
    if (done) return;
    const lines = (buffered + value).split("\\n");
    buffered = lines.pop();
    for (const line of lines) if (line) yield JSON.parse(line);
  }
}
const transport = {
  async download() {
    const url = base + "/download?bytes=1048576";
    return { bytes: await drain((await fetched(url)).body), protocol: protocol(url) };
  },
  async upload() {
    const url = base + "/upload?id=" + await uploadId();
    const response = await fetched(url, { method: "POST", body: new Uint8Array(1048576) });
    return { bytes: (await response.json()).bytes, protocol: protocol(url) };
  },
  wt_ping: () => session("/wt/ping", async (wt) => {
    await wt.datagrams.writable.getWriter().write(new TextEncoder().encode("PING,7"));
    const { value } = await within(wt.datagrams.readable.getReader().read(), 5000);
    return new TextDecoder().decode(value);
  }),
  wt_download: () => session("/wt/download?bytes=65536", async (wt) => {
    const { value } = await within(wt.incomingUnidirectionalStreams.getReader().read(), 5000);
    return await within(drain(value), 5000);
  }),
  wt_upload: async () => session("/wt/upload?id=" + await uploadId(), async (wt) => {
    const { value } = await within(wt.incomingUnidirectionalStreams.getReader().read(), 5000);
    const feed = records(value);
    if ((await within(feed.next(), 5000)).value?.type !== "ready") throw new Error("no ready record");
    const writer = (await wt.createUnidirectionalStream()).getWriter();
    await writer.write(new Uint8Array(65536));
    await writer.close();
    for (;;) {
      const { value: record, done } = await within(feed.next(), 5000);
      if (done) throw new Error("progress feed ended");
      if (record.type === "progress" && record.bytes >= 65536) return record.bytes;
    }
  }),
  async wt_close() {
    const wt = new WebTransport(base + "/wt/ping", options);
    await within(wt.ready, 5000);
    return await within(wt.closed, 10000);
  },
};
const negative = {
  webtransport: async () => typeof WebTransport,
  async download() {
    const url = base + "/download?bytes=65536";
    return { bytes: await drain((await fetched(url)).body), protocol: protocol(url) };
  },
};
(async () => {
  const report = { ua: navigator.userAgent, checks: {} };
  for (const [name, check] of Object.entries(query.has("negative") ? negative : transport)) {
    try { report.checks[name] = { ok: true, detail: await within(check(), 20000) }; }
    catch (error) { report.checks[name] = { ok: false, detail: `${error.name}: ${error.message}` }; }
  }
  await fetch("/report", { method: "POST", body: JSON.stringify(report) });
})();
</script>"""


def run(command: list[str], **kwargs) -> str:
    return subprocess.run(command, check=True, capture_output=True, text=True, **kwargs).stdout


def identity(directory: Path) -> tuple[Path, Path, Path]:
    """A test CA and a 10-day P-256 leaf, short enough for serverCertificateHashes."""
    ca, leaf, key = directory / "ca.pem", directory / "leaf.pem", directory / "leaf.key"
    ec = ["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes"]
    run(["openssl", "req", "-x509", *ec, "-days", "10", "-subj", "/CN=gm browser transports CA",
         "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign",
         "-keyout", str(directory / "ca.key"), "-out", str(ca)])
    run(["openssl", "req", *ec, "-subj", "/CN=127.0.0.1", "-keyout", str(key), "-out", str(directory / "leaf.csr")])
    (directory / "leaf.ext").write_text(
        "subjectAltName=IP:127.0.0.1,IP:127.0.0.2\nextendedKeyUsage=serverAuth\nbasicConstraints=critical,CA:FALSE\n")
    run(["openssl", "x509", "-req", "-in", str(directory / "leaf.csr"), "-CA", str(ca),
         "-CAkey", str(directory / "ca.key"), "-set_serial", "1", "-days", "10",
         "-extfile", str(directory / "leaf.ext"), "-out", str(leaf)])
    return ca, leaf, key


class Pages(http.server.ThreadingHTTPServer):
    """Serves the page on 127.0.0.2: Firefox's h3 mapping for 127.0.0.1 ignores ports."""

    def __init__(self, context: ssl.SSLContext) -> None:
        super().__init__(("127.0.0.2", PAGE), Handler)
        self.context, self.report, self.received = context, {}, threading.Event()

    def get_request(self) -> tuple[socket.socket, object]:
        # The handshake runs in the handler thread, so a stalled client cannot block accept.
        connection, address = self.socket.accept()
        return self.context.wrap_socket(connection, server_side=True, do_handshake_on_connect=False), address


class Handler(http.server.BaseHTTPRequestHandler):
    server: Pages

    def do_GET(self) -> None:
        body = PAGE_HTML.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        self.server.report = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.send_response(204)
        self.end_headers()
        self.server.received.set()

    def log_message(self, *_: object) -> None:
        pass


def start(command: list[str], log: Path, env: dict[str, str]) -> subprocess.Popen:
    with log.open("w") as output:
        return subprocess.Popen(command, stdout=output, stderr=subprocess.STDOUT, env=env, start_new_session=True)


def stop(process: subprocess.Popen) -> None:
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(process.pid, sig)
            process.wait(timeout=5)
            return
        except ProcessLookupError:
            return
        except subprocess.TimeoutExpired:
            continue


def browse(pages: Pages, command: list[str], log: Path, env: dict[str, str]) -> dict:
    pages.received.clear()
    browser = start(command, log, env)
    try:
        if not pages.received.wait(90):
            raise TimeoutError("the page reported nothing")
        return pages.report
    finally:
        stop(browser)


def page(base: int, *query: str) -> str:
    return f"https://127.0.0.2:{PAGE}/?base=https://127.0.0.1:{base}" + "".join(f"&{item}" for item in query)


def chromium(directory: Path, pages: Pages, leaf: Path) -> dict:
    public_key = run(["openssl", "x509", "-in", str(leaf), "-pubkey", "-noout"]).encode()
    spki = subprocess.run(["openssl", "pkey", "-pubin", "-outform", "der"], input=public_key,
                          check=True, capture_output=True).stdout
    certificate = hashlib.sha256(ssl.PEM_cert_to_DER_cert(leaf.read_text())).digest()
    return browse(pages, [
        CHROMIUM, "--headless=new", "--no-sandbox", "--no-first-run", "--disable-gpu",
        f"--user-data-dir={directory / 'chromium'}",
        f"--ignore-certificate-errors-spki-list={base64.b64encode(hashlib.sha256(spki).digest()).decode()}",
        f"--origin-to-force-quic-on=127.0.0.1:{H3}", page(H3, "hash=" + ",".join(map(str, certificate))),
    ], directory / "chromium.log", {**os.environ, "HOME": str(directory)})


def firefox(directory: Path, pages: Pages, ca: Path) -> dict:
    profile = directory / "firefox"
    profile.mkdir()
    run(["certutil", "-N", "-d", f"sql:{profile}", "--empty-password"])
    run(["certutil", "-A", "-d", f"sql:{profile}", "-n", "gm-test-ca", "-t", "C,,", "-i", str(ca)])
    preferences = {
        "network.http.http3.enable": True,
        "network.http.http3.alt-svc-mapping-for-testing": f"127.0.0.1;h3=:{H3}",
        "network.http.http3.disable_when_third_party_roots_found": False,
        "network.webtransport.enabled": True,
        "network.webtransport.datagrams.enabled": True,
        "browser.shell.checkDefaultBrowser": False,
        "datareporting.policy.dataSubmissionEnabled": False,
        "toolkit.telemetry.reportingpolicy.firstRun": False,
        "app.update.disabledForTesting": True,
    }
    (profile / "user.js").write_text("".join(f"user_pref({json.dumps(name)}, {json.dumps(value)});\n"
                                             for name, value in preferences.items()))
    return browse(pages, [FIREFOX, "--headless", "--no-remote", "--profile", str(profile), page(H3)],
                  directory / "firefox.log", {**os.environ, "HOME": str(directory), "MOZ_HEADLESS": "1"})


def webkit(directory: Path, pages: Pages) -> dict:
    home = directory / "webkit"
    home.mkdir()
    (home / "bus.conf").write_text(BUS)
    # Unix socket paths are limited to 108 bytes, so the runtime directory stays short.
    with tempfile.TemporaryDirectory(prefix="gm-wk-") as runtime:
        environment = {
            "PATH": os.environ["PATH"], "HOME": str(home), "XDG_RUNTIME_DIR": runtime,
            "XDG_CACHE_HOME": str(home / "cache"), "XDG_DATA_HOME": str(home / "data"),
            "XDG_CONFIG_HOME": str(home / "config"), "GDK_BACKEND": "broadway", "BROADWAY_DISPLAY": ":5",
            "NO_AT_BRIDGE": "1", "GTK_A11Y": "none", "GIO_USE_VFS": "local", "GTK_USE_PORTAL": "0",
        }
        display = start(["gtk4-broadwayd", "--unixsocket", f"{runtime}/broadway-http", ":5"],
                        directory / "broadway.log", environment)
        try:
            return browse(pages, ["dbus-run-session", f"--config-file={home / 'bus.conf'}", "--",
                                  MINIBROWSER, "--ignore-tls-errors", page(H2, "negative")],
                          directory / "webkit.log", environment)
        finally:
            stop(display)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--browsers", default="chromium,firefox,webkit")
    args = parser.parse_args()
    target = ROOT / "rust/target"
    target.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="browser-transports-", dir=target))
    print(f"Evidence: {directory}", flush=True)
    ca, leaf, key = identity(directory)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(leaf, key)
    pages = Pages(context)
    threading.Thread(target=pages.serve_forever, daemon=True).start()
    server = start([str(args.server.resolve()), "--h1-addr=127.0.0.1:0", "--h1-tls-addr=",
                    f"--h2-addr=127.0.0.1:{H2}", f"--h3-addr=127.0.0.1:{H3}",
                    f"--tls-cert={leaf}", f"--tls-key={key}"], directory / "server.log",
                   {"PATH": os.environ["PATH"], "HOME": str(directory), "GM_AUTH_MODE": "off",
                    "GM_MAX_OPERATION_DURATION": "2s", "GM_MAX_SESSION_DURATION": "3s"})
    runners = {"chromium": lambda: chromium(directory, pages, leaf),
               "firefox": lambda: firefox(directory, pages, ca),
               "webkit": lambda: webkit(directory, pages)}
    failed = []
    try:
        deadline = time.monotonic() + 15
        while True:
            with socket.socket() as probe:
                if probe.connect_ex(("127.0.0.1", H2)) == 0:
                    break
            if server.poll() is not None or time.monotonic() > deadline:
                raise SystemExit(f"server did not start: {(directory / 'server.log').read_text()}")
            time.sleep(0.05)
        for browser in args.browsers.split(","):
            try:
                report = runners[browser]()
            except (OSError, TimeoutError, subprocess.CalledProcessError) as error:
                report = {"ua": str(error), "checks": {}}
            (directory / f"{browser}.json").write_text(json.dumps(report, indent=1))
            print(f"{browser}: {report['ua']}", flush=True)
            for name, accept in (NEGATIVE_CHECKS if browser == "webkit" else TRANSPORT_CHECKS).items():
                check = report["checks"].get(name, {"ok": False, "detail": "missing"})
                passed = check["ok"] and accept(check["detail"])
                print(f"  {'pass' if passed else 'FAIL'} {name}: {json.dumps(check['detail'])}", flush=True)
                if not passed:
                    failed.append(f"{browser} {name}")
    finally:
        stop(server)
        pages.shutdown()
    if failed:
        raise SystemExit(f"failed: {', '.join(failed)}")


if __name__ == "__main__":
    main()
