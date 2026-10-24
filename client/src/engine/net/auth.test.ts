import { afterEach, expect, setSystemTime, test } from "bun:test";
import type { CatalogEntry } from "../../protocol/documents";
import { SignInRequired, type RequestRecipe, type ServerAuth } from "../types";
import { approvalChallenge, beginApproval, exchangeGrant, grantAuth, initialAuth } from "./auth";
import { fakeNet, MARKER, page as publicPage, type Answer } from "./fakes.testutil";
import { csrfHeaders, type PageConfig, type PageSession } from "./page";

const page: PageConfig = { ...publicPage, auth: true };
const self: CatalogEntry = {
  id: "self",
  url: page.origin,
  name: "Meter",
  additionalOrigins: ["https://cdn.example"],
};
const peer: CatalogEntry = {
  id: "peer",
  url: "https://peer.example",
  name: "Peer",
  additionalOrigins: ["https://cdn.example:8443"],
};
const session = {
  headers: (method: string) => csrfHeaders(method, "theme=dark; __Host-gm_csrf=a%2Bb"),
} as PageSession;
const publicAuth = (entry: CatalogEntry) => initialAuth(entry, page, session);
const sessionAuth = (entry: CatalogEntry, config: PageConfig, login: PageSession) =>
  initialAuth(entry, config, login);
const TOKEN = "t".repeat(43);
/** A grant whose monotonic deadline is `deadline` and whose wall-clock expiry is `expires`. */
const held = (deadline: number, expires = Infinity) => ({
  token: TOKEN,
  deadline,
  expires,
  maximumLifetimeMs: 28_800_000,
});
const grant = (deadline: number) => grantAuth(peer, held(deadline), () => 0);

const OUTSIDE = "Request destination is outside the selected server";
const omit: RequestRecipe = { headers: {}, credentials: "omit", redirect: "error" };
const bearer: RequestRecipe = {
  headers: { Authorization: `Bearer ${TOKEN}` },
  credentials: "omit",
  redirect: "error",
};
const cookie = (headers = {}): RequestRecipe => ({
  headers,
  credentials: "include",
  redirect: "error",
});

test("credentials stay on their server's destinations, a grant on its HTTPS hostname, cookies on the page's", () => {
  const recipe = (auth: ServerAuth, url: string, method: string): unknown => {
    try {
      return auth.request(url, method);
    } catch (error) {
      return (error as Error).message;
    }
  };
  for (const [auth, url, method, expected] of [
    [publicAuth(peer), "https://peer.example:7249/probe?cb=1", "GET", omit],
    [publicAuth(peer), "http://peer.example:7246/download", "GET", omit],
    [publicAuth(peer), "https://cdn.example:8443/probe", "GET", omit],
    [publicAuth(peer), "https://cdn.example:9443/probe", "GET", OUTSIDE],
    [publicAuth(peer), "https://edge.peer.example/probe", "GET", OUTSIDE],
    [publicAuth(peer), "https://peer.example.evil.example/probe", "GET", OUTSIDE],
    [publicAuth(peer), "wss://peer.example/ws/ping", "GET", OUTSIDE],
    [grant(1), "https://peer.example:7249/upload", "POST", bearer],
    [grant(1), "http://peer.example:7246/probe", "GET", "Measurement grants require HTTPS"],
    [
      grant(1),
      "https://cdn.example:8443/probe",
      "GET",
      "Measurement grants stay on their server's hostname",
    ],
    [grant(0), "https://peer.example/probe", "GET", "Sign in to Peer"],
    [sessionAuth(self, page, session), "https://meter.example/probe", "GET", cookie()],
    [sessionAuth(self, page, session), "https://meter.example/probe", "HEAD", cookie()],
    [
      sessionAuth(self, page, session),
      "https://meter.example/lease",
      "post",
      cookie({ "X-CSRF-Token": "a+b" }),
    ],
    [
      sessionAuth(self, page, session),
      "https://meter.example:7249/upload",
      "POST",
      cookie({ "X-CSRF-Token": "a+b" }),
    ],
    [
      sessionAuth(self, page, session),
      "http://meter.example:7246/probe",
      "GET",
      "Session credentials stay on this page's secure hostname",
    ],
    [
      sessionAuth(self, page, session),
      "https://cdn.example/probe",
      "GET",
      "Session credentials stay on this page's secure hostname",
    ],
  ] as const)
    expect(recipe(auth, url, method), `${auth.kind} ${method} ${url}`).toEqual(expected);
});

test("only the page's own server at the page's origin rides the page's login", () => {
  for (const [entry, auth, kind] of [
    [self, true, "session"],
    [self, false, "public"],
    [{ ...self, url: "https://meter.example:8443" }, true, "public"],
    [{ ...peer, url: page.origin, id: "home" }, true, "public"],
  ] as const)
    expect(initialAuth(entry, { ...page, auth }, session).kind).toBe(kind);
});

test("the comparison code is base32 of the verifier hash's first five bytes, and the verifier stays here", async () => {
  for (const [verifier, challenge, code] of [
    [
      "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
      "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
      "CPJR5FQ2",
    ],
    ["A".repeat(43), "DwBzhbb51LfusnSGBa_hqYSgo7-j8BTQnip4TOnlzRo", "B4AHHBNW"],
  ] as const)
    expect(await approvalChallenge(verifier)).toEqual({ challenge, code });
  const first = await beginApproval(peer, page);
  const second = await beginApproval(peer, page);
  expect(first.verifier).not.toBe(second.verifier);
  expect(first.code).toMatch(/^[A-Z2-7]{8}$/);
  expect(first.url).not.toContain(first.verifier);
  const { challenge, code } = await approvalChallenge(first.verifier);
  expect(first.code).toBe(code);
  expect(first.url).toBe(
    `https://peer.example/auth/browser?challenge=${challenge}&client_origin=https%3A%2F%2Fmeter.example`,
  );
  for (const [entry, browser] of [
    [peer, { ...page, secure: false, origin: "http://meter.example" }],
    [{ ...peer, url: "http://peer.example" }, page],
  ] as const)
    await expect(beginApproval(entry, browser)).rejects.toThrow(
      "Open this interface over HTTPS to sign in to a remote server.",
    );
});

let net: ReturnType<typeof fakeNet> | undefined;
afterEach(() => net?.restore());

test("a marked refusal is a sign-in that ends what it refused; an unmarked one is an answer", async () => {
  net = fakeNet((url) =>
    url.pathname === "/marked" ? { status: 403, headers: MARKER } : { status: 403 },
  );
  expect((await publicAuth(peer).fetch("https://peer.example/plain")).status).toBe(403);
  await expect(publicAuth(peer).fetch("https://peer.example/marked")).rejects.toBeInstanceOf(
    SignInRequired,
  );
  const ended: string[] = [];
  const ending = { ...session, end: (reason: string) => ended.push(reason) } as PageSession;
  await expect(
    sessionAuth(self, page, ending).fetch(`${page.origin}/marked`),
  ).rejects.toBeInstanceOf(SignInRequired);
  expect(ended).toEqual(["expired"]);
  // A refused grant is over: whatever follows is refused before any request.
  const auth = grantAuth(peer, held(60_000), net.now);
  await expect(auth.fetch("https://peer.example/marked")).rejects.toBeInstanceOf(SignInRequired);
  const sent = net.requests.length;
  expect(() => auth.request("https://peer.example/probe", "GET")).toThrow(SignInRequired);
  expect(await auth.ended()).toBe(true);
  expect(net.requests.length).toBe(sent);
});

test("whether a grant ended is one 3 s probe for all who ask; only its marker or its deadline says yes", async () => {
  let answer: Answer = "unreachable";
  net = fakeNet(() => answer);
  const auth = grantAuth(peer, held(60_000), net.now);
  for (const [next, ended] of [
    ["unreachable", false],
    [{ status: 403 }, false],
    ["hang", false],
    [{ status: 403, headers: MARKER, afterMs: 100 }, true],
  ] as const) {
    answer = next;
    const asked = Promise.all([auth.ended(), auth.ended()]);
    await net.advance(3_000);
    expect(await asked, JSON.stringify(next)).toEqual([ended, ended]);
  }
  expect(
    net.requests.map(({ url, init }) => [
      url.href,
      init.credentials,
      new Headers(init.headers).get("Authorization"),
    ]),
  ).toEqual(Array(4).fill(["https://peer.example/probe", "omit", `Bearer ${TOKEN}`]));
  expect(() => auth.request("https://peer.example/probe", "GET")).toThrow(SignInRequired);
  // A grant past its deadline has ended without asking.
  const late = grantAuth(peer, held(9_000), net.now);
  expect(await late.ended()).toBe(true);
  expect(net.requests.length).toBe(4);
});

test("a grant also ends by the wall clock, which runs on while a suspend pauses the page clock", async () => {
  const slept = grantAuth(peer, held(60_000, Date.now() + 1_000), () => 0);
  expect(slept.request("https://peer.example/probe", "GET")).toEqual(bearer);
  setSystemTime(new Date(Date.now() + 1_000));
  try {
    expect(() => slept.request("https://peer.example/probe", "GET")).toThrow(SignInRequired);
    expect(await slept.ended()).toBe(true);
  } finally {
    setSystemTime();
  }
});

test("the grant exchange polls until approval and refuses invalid grants, the login's limit and an abort", async () => {
  const MAX = 9 * 3_600_000;
  const grant = (remainingMs: number, token = TOKEN): Answer => ({
    body: { token, remainingMs, maximumLifetimeMs: MAX },
  });
  const invalid = "Could not read the measurement grant.";
  for (const [answers, outcome] of [
    [
      [{ status: 202 }, "unreachable", { status: 503 }, grant(3_600_000)],
      { token: TOKEN, deadline: 3_603_000, maximumLifetimeMs: MAX },
    ],
    [[grant(0)], invalid],
    [[grant(8 * 3_600_000 + 1)], invalid],
    [[grant(3_600_000, "short")], invalid],
    [[{ status: 429 }], "This server's sign-in has reached its limit of authorized clients."],
    [[{ status: 400 }], "Could not finish the sign-in (HTTP 400)."],
    [[{ status: 202 }], "Approval expired. Sign in again."],
  ] as const) {
    net?.restore();
    let round = 0;
    net = fakeNet(() => answers[Math.min(round++, answers.length - 1)]!);
    const wall = Date.now() - net.now();
    const signal = new AbortController().signal;
    const exchanged = exchangeGrant(peer, "verifier", signal, net.now).catch(
      (error: Error) => error.message,
    );
    await net.advance(130_000);
    // The wall-clock expiry is the monotonic deadline on the clock the page started on.
    const expected =
      typeof outcome === "string" ? outcome : { ...outcome, expires: wall + outcome.deadline };
    expect(await exchanged, JSON.stringify(answers)).toEqual(expected);
  }
  expect(net?.requests[0]?.init).toMatchObject({
    method: "POST",
    body: JSON.stringify({ verifier: "verifier" }),
    credentials: "omit",
    redirect: "error",
  });
  // A grant whose answer was lost, asked for again past the server's replay window, ends the polling at once.
  net?.restore();
  let rounds = 0;
  net = fakeNet(() => (rounds++ ? { status: 410 } : "unreachable"));
  const lost = exchangeGrant(peer, "verifier", new AbortController().signal, net.now).catch(
    (e: Error) => e.message,
  );
  await net.advance(1_500);
  expect({ outcome: await lost, requests: net.sent("/auth/browser/token") }).toEqual({
    outcome: "The approval's answer was lost on the way. Sign in again.",
    requests: [0, 1_000],
  });
  // Canceling sign-in stops the polling at once.
  net?.restore();
  net = fakeNet(() => ({ status: 202 }));
  const cancel = new AbortController();
  const exchanged = exchangeGrant(peer, "verifier", cancel.signal, net.now).catch(
    (error: Error) => error.name,
  );
  await net.advance(1_500);
  cancel.abort();
  await net.advance(5_000);
  expect({ outcome: await exchanged, requests: net.sent("/auth/browser/token") }).toEqual({
    outcome: "AbortError",
    requests: [0, 1_000],
  });
});
