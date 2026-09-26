"""Approve a disposable native-client challenge in the Go interop fixture."""

import http.cookiejar
import re
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request

origin, approval_url, ca_path = sys.argv[1:]
source = urllib.parse.urlparse(origin)
approval = urllib.parse.urlparse(approval_url)
port = source.netloc.removeprefix("127.0.0.1:")
if (source.scheme, source.hostname, approval.path) != ("https", "127.0.0.1", "/auth/cli") \
        or approval.netloc != source.netloc:
    raise SystemExit("approval fixture only approves its own loopback server")
if not port.isdigit():
    raise SystemExit("approval fixture received no loopback port")
base = f"https://127.0.0.1:{port}"
challenge = urllib.parse.parse_qs(approval.query).get("challenge", [""])[0]
if not re.fullmatch(r"[A-Za-z0-9_-]{43}", challenge):
    raise SystemExit("approval fixture received no valid challenge")

cookies = http.cookiejar.CookieJar()
context = ssl.create_default_context(cafile=ca_path)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        return None


opener = urllib.request.build_opener(
    urllib.request.ProxyHandler({}),
    urllib.request.HTTPSHandler(context=context),
    urllib.request.HTTPCookieProcessor(cookies),
    NoRedirect(),
)


def request(method, url, form=None, expected=200):
    headers = {"Origin": base, "Sec-Fetch-Site": "same-origin"}
    body = None
    if form is not None:
        body = urllib.parse.urlencode(form).encode()
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    operation = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        response = opener.open(operation, timeout=10)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        response.read(64 * 1024)
        if response.status != expected:
            raise RuntimeError(f"{method} {urllib.parse.urlparse(url).path}: HTTP {response.status}, expected {expected}")


def cookie(name):
    return next((item.value for item in cookies if item.name == name), None)


request("GET", base + "/login")
nonce = cookie("__Host-gm_login")
if not nonce:
    raise SystemExit("login fixture received no nonce cookie")
request(
    "POST", base + "/auth/password",
    {"csrf": nonce, "password": "correct horse battery staple"},
    expected=303,
)
csrf = cookie("__Host-gm_csrf")
if not cookie("__Host-gm_session") or not csrf:
    raise SystemExit("password fixture received no session or CSRF cookie")
request("GET", base + "/auth/cli?" + urllib.parse.urlencode({"challenge": challenge}))
request("POST", base + "/auth/cli/approve", {"csrf": csrf, "challenge": challenge})
