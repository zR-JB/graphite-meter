"""Approve a disposable native-client challenge in the Go interop fixture."""

import http.cookiejar
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request

base, approval_url, ca_path = sys.argv[1:]
source = urllib.parse.urlparse(base)
approval = urllib.parse.urlparse(approval_url)
if source.scheme != "https" or approval.scheme != "https" or source.netloc != approval.netloc:
    raise SystemExit("approval fixture received an unexpected browser origin")
challenge = urllib.parse.parse_qs(approval.query).get("challenge", [""])[0]
if not challenge:
    raise SystemExit("approval fixture received no challenge")

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
request("GET", approval_url)
request("POST", base + "/auth/cli/approve", {"csrf": csrf, "challenge": challenge})
