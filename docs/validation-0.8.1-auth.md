# Independent-server authentication validation for 0.8.1

This deployment pass addresses [#184](https://github.com/zR-JB/graphite-meter/issues/184).
It found and fixed two browser behaviors that the existing mocked tests missed:

- A requester cannot reliably close a cross-origin popup after clearing its
  opener. Chrome left both completed password and OIDC popups open. Sign in now
  presents an explicit **Open sign-in page** link with `noopener noreferrer`.
  The user owns that tab; the application owns and cancels only its exchange.
- A cross-site approval navigation omitted an existing `SameSite=Strict` session
  cookie. The unnecessary second password login revoked the first client's
  grant: its measurement request changed from 200 to 403. A same-origin document
  continuation now makes the existing cookie available before deciding whether
  login is needed. Cookies remain Strict, and every new grant still requires
  explicit approval of the exact requesting origin and matching code.

## Revision and deployment

Baseline reproduction used `f6f32a3f51b229c4b45fae67886e41eb6f6e58cc`.
The fixes are `a3fa0ed6bca283fef3ca3526cff1999dd79c8c44` and
`2fb77c2f681da470bc2a27096020eaaabf95c957`. Validation took place on
2026-09-08. The final candidate was built from the latter commit with
`vcs.modified=false`, Go 1.27.1, Bun 1.4.2, production browser assets, and client
and server labels `2fb77c2`. Its SHA-256 is
`8e9a1f9d6251b60b2a938da8e3cb08522cba8abf572e7e03238a69b8a7534a6c`.
The accompanying [sanitized evidence](validation-0.8.1-auth-evidence.json)
distinguishes the broader development matrix from the exact-commit replay.

Three independent Graphite Meter processes and a separately configured Keycloak
process ran on loopback, using different sites rather than different subdomains
of one site:

| Role | HTTPS origin | Configuration |
| --- | --- | --- |
| Requesting interface | `https://requester184.test:7443` | Public; catalogue selects both protected peers |
| Password peer | `https://password184.test:8443` | Independent operator password; native WebTransport at port 8445 |
| OIDC peer | `https://oidc184.test:9443` | Independent OIDC session; native WebTransport at port 9445 |
| OIDC provider | `https://provider184.test:9444/realms/auth184` | Keycloak 26.3.3, separate validation user, `meter-users` group |

Keycloak used image digest
`sha256:6a7217a100bd3e5de4063a27a538ef999a3c5a88c4b4ec0ffc0a642aee7b2597`.
The confidential client used the exact callback
`https://oidc184.test:9443/auth/oidc/callback`, authorization code flow,
`client_secret_basic`, PKCE S256, and `openid profile groups` scopes. A group
membership mapper supplied the allowed group in identity and UserInfo claims.
Password and OIDC credentials were generated independently and kept outside the
tracked checkout. No peer shared an authentication secret, parent session, or
measurement grant.

The fixture used a shared test-only TLS certificate with the named sites in its
SAN list. Server-to-provider HTTPS verified that certificate through an explicit
CA file. Chrome trusted its exact SPKI; Firefox used the driver's certificate
exception for this isolated fixture. This validates cross-site authentication
and session ownership, not public DNS/CA deployment, WAN performance, or an
independently administered production identity provider.

Browser runs used Google Chrome 152.0.7977.82 with Playwright 1.55.0 and Firefox
155.0 with the matching Playwright 1.63.0 browser build. Chrome enabled
third-party-cookie restrictions and retained normal popup blocking. Firefox set
`network.cookie.cookieBehavior=1` and enabled popup blocking. A nonsecret
`SameSite=None; Secure` probe cookie confirmed enforcement: Chrome reported
`UserPreferences` as its blocked reason; Firefox sent no Cookie header on the
cross-site probe. Successful login tabs had `window.opener === null`.

## Observed deployment results

| Scenario | Observation |
| --- | --- |
| Password and real OIDC approval | Both browsers displayed the exact requesting origin and matching eight-character codes. Neither relied on opener access or third-party cookies. |
| Ordinary second-client approval | Chrome and Firefox reused both kinds of parent login. First and second grants each returned 200; neither password entry nor an OIDC login was requested again. |
| Eight ordinary requesting tabs | Eight approvals required one password login. The first grant remained valid. The ninth client displayed explicit renewal in both the peer tab and requesting interface. |
| Explicit renewal | All eight old grants returned 403. Active password-peer download and WebSocket ping ended. The OIDC peer kept transferring, answering pings, and returning 200. A fresh password-peer approval succeeded. |
| Fetch and WebSocket measurement | Both protected peers completed idle latency, download, upload, and bidirectional stages with no failures. Successful bearer responses had no Cookie header; WebSocket URLs contained only short-lived ticket credentials. |
| WebTransport measurement | Both protected peers completed all four stages over HTTP/3 WebTransport throughput and latency, with no failures. |
| Wrong password | The real form showed “Incorrect password. Check it and try again.” and retained the retry field; the correct password then worked. |
| Declined or abandoned approval | Closing the approval tab without approving yielded no grant. After the real two-minute exchange deadline, the requester showed expiry/retry guidance. Retry used a different code and succeeded. |
| Cancel before login | Polling stopped. Completing login and approving the old tab later delivered no grant and started no measurement. |
| Deselect, reload, or dispose before approval | Owned polling stopped at each stage. Late approval neither restored the selection nor started a run. Reload after successful approval required fresh peer approval. |
| Cancel after the server issued a grant | The driver held the real successful exchange response, canceled the requester, then released it. No bearer was installed or used, no further polling occurred, and no measurement started. |
| Logout during download or upload with loaded latency | The password peer's latency and transfer resources ended. Saved results retained its identity and failure evidence, began a survivor-only interval, and continued with the OIDC peer. Settings identified **Password validation** for sign-in. |
| Grant issuer and requesting origin | Valid measurement grants returned 200. Wrong issuer and wrong exact requesting origin returned 403 for both peers. |
| Redirect refusal | An authenticated preflight was given a controlled 307 response. The browser made zero requests to its redirect destination and presented retry guidance. |
| Socket tickets | First WebSocket use succeeded; replay failed. Wrong destination returned 403 and consumed the ticket. Wrong origin and wrong WebTransport route failed. An unused ticket failed after its real 30-second lifetime. |
| Upload ownership | One grant's upload write returned 200. Another grant's read, delete, and write each returned 403; the owner's delete returned 204. |
| Public approval quota | With one real OIDC approval already pending, 255 additional public approvals were admitted and 15 further requests were refused. The waiting Keycloak login/callback and existing password grant still completed successfully. After 123 seconds, fresh admission and both existing grants returned 200. |
| Reusable credentials | Observed measurement requests omitted cookies. Reusable grants were absent from socket URLs, localStorage, and persisted result history. A scan for the current grants, passwords, OIDC client secret, and session cookie found no matches in ordinary server logs. |

The quota fill used 270 unauthenticated HTTPS requests across 27 distinct
loopback source addresses, respecting each address's ten-attempt limit while
reaching the separate 256-approval global bound. No auth counters, clocks, or
production resource limits were patched to obtain these results. The delayed
response and redirect cases intercepted only the specified browser response;
login, approval, token issuance, and the other measurement requests still used
the real servers.

## Regression coverage and release disposition

[Engine auth tests](../client/src/lib/runner/engineAuth.test.ts) now reject
unsolicited popup creation and cover cancellation while a successful grant
response is already in flight. [Server approval tests](../go/internal/auth/browser_test.go)
cover the same-origin continuation, preserved parent login and first grant,
mandatory manual approval, and ordinary login when there is no cookie or usable
navigation metadata. Non-navigation requests retain their prior behavior.

Applicable local gates passed: all Go tests; Go formatting and vet, including
stress-tag vet; Staticcheck 2026.2.1; all auth race tests and the selected
server/auth lifecycle race matrix; all 820 browser-source tests; browser types,
formatting, generated types, dependency deduplication, generated authentication
assets, Gitleaks, and legal closure checks (server/browser 17, TUI 31, container
18).

The popup checklist has an explicit 0.8.1 design disposition: there is no
application-owned cross-origin popup to close. The isolated sign-in tab remains
user-owned, and the UI and [recovery guidance](SERVERS.md#independent-sign-in)
say so. Cancellation must stop the owned exchange and prevent late activation;
those properties were exercised in both browsers.

The production eight-hour parent-session deadline was **not** awaited in a
wall-clock soak. Expiry coverage uses `TestBrowserGrantCannotOutliveItsParentLogin`
and `TestWebTransportTokensDieWithAnExpiredSession`, including the production
expiry cleanup and cancellation authority. The deployment runs exercised real
logout and renewal through that same resource ownership boundary, plus actual
two-minute approval and 30-second ticket expiry. This is the 0.8.1 expiry
validation disposition; it is not evidence of an eight-hour browser run or
private-map memory profiling.

The matrix found no remaining auth defect after these fixes. Closing #184 still
requires the integrated revision's applicable gates, resolved CodeQL review,
and a key-flow replay on the integrated build; those release checks are distinct
from this candidate deployment report.

## Integrated-build replay

The key flows were repeated on 2026-09-08 against
`e1cbcf69642f0d357c1d58a240521edf51f46da6`, built with
`vcs.modified=false`. The supplied production binary had SHA-256
`171923487730ffaa01283c6ef478ed398439914ba2bf5d623e151555ae38888c`;
all three deployed servers identified themselves as `e1cbcf6`.
The independent HTTPS/password/Keycloak fixture and cookie settings above were
retained.

- Chrome 152.0.7977.82 and Firefox 155.0 each authorized two requesting interfaces
  against both protected peers. The second authorization reused each existing
  parent login, both old and new grants returned 200, and sign-in tabs had no opener.
- Canceling after real server issuance but before response delivery stopped
  polling at two requests. Releasing the response installed no usable grant and
  started no measurement.
- Both peers completed idle latency, download, upload, and bidirectional stages
  over fetch/WebSocket (5,773,123,347 transferred bytes) and WebTransport
  (638,486,218 bytes), with no failures.
- The sampled successful bearer responses numbered 170 and 72 respectively;
  none carried cookies. Wrong requesting origins and issuers returned 403.
  Reusable grants were absent from socket URLs, localStorage, and saved history.

These results supplement the broader lifecycle matrix. They do not extend its
stated eight-hour-soak, public-provider, or private-map-profiling coverage.
Integrated repository gates and CodeQL review are recorded separately.
