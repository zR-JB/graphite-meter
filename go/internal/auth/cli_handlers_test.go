package auth

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json/v2"
	"fmt"
	"maps"
	"net/http"
	"net/http/httptest"
	"net/url"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/testkit"
)

// challengeFor returns the base64url challenge a terminal client derives from a verifier.
func challengeFor(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func cliPageRequest(challenge, cookie string) *http.Request {
	r := secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil)
	if cookie != "" {
		withSessionCookie(r, cookie)
	}
	return r
}

func cliExchange(s *Service, body string) *httptest.ResponseRecorder {
	return serveMounted(s, secureRequest(http.MethodPost, "/auth/cli/token", strings.NewReader(body)))
}

func mountedAuth(s *Service) http.Handler {
	mux := http.NewServeMux()
	s.Mount(mux)
	return s.Enforce(mux, Listener{UI: true})
}

func serveMounted(s *Service, r *http.Request) *httptest.ResponseRecorder {
	return testkit.Record(mountedAuth(s).ServeHTTP, r)
}

func approvalForm(path, challenge, cookie string, sess *session) *http.Request {
	form := url.Values{"challenge": {challenge}, "csrf": {sess.csrf}}.Encode()
	r := withSessionCookie(secureRequest(http.MethodPost, path, strings.NewReader(form)), cookie)
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", "https://meter.example")
	return r
}

// approveNative opens the terminal approval page and confirms it as the signed-in operator does.
func approveNative(t *testing.T, s *Service, cookie string, sess *session, verifier string) {
	t.Helper()
	challenge := challengeFor(verifier)
	if w := serveMounted(s, cliPageRequest(challenge, cookie)); w.Code != http.StatusOK {
		t.Fatalf("approval page = %d, want 200", w.Code)
	}
	if w := cliExchange(s, `{"verifier":"`+verifier+`"}`); w.Code != http.StatusAccepted {
		t.Fatalf("exchange before the confirming click = %d, want 202", w.Code)
	}
	if w := serveMounted(s, approvalForm("/auth/cli/approve", challenge, cookie, sess)); w.Code != http.StatusOK {
		t.Fatalf("approve = %d, want 200", w.Code)
	}
}

func nativeGrant(t *testing.T, s *Service, cookie string, sess *session, verifier string) string {
	t.Helper()
	approveNative(t, s, cookie, sess, verifier)
	w := cliExchange(s, `{"verifier":"`+verifier+`"}`)
	var out struct {
		Token string `json:"token"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &out); w.Code != http.StatusOK || err != nil || out.Token == "" {
		t.Fatalf("exchange = %d %s", w.Code, w.Body.String())
	}
	return out.Token
}

func TestCliPageRefusals(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local")
	grant := grantFor(t, s, sess)
	bearer := cliPageRequest(challengeFor("verifier-bearer"), "")
	bearer.Header.Set("Authorization", "Bearer "+grant)
	for name, tc := range map[string]struct {
		r    *http.Request
		want int
	}{
		"invalid challenge": {cliPageRequest("not-a-challenge", raw), http.StatusForbidden},
		"no session":        {cliPageRequest(challengeFor("verifier-abc"), ""), http.StatusSeeOther},
		"bearer principal":  {bearer, http.StatusSeeOther},
	} {
		rr := testkit.Record(s.cliPage, tc.r)
		if rr.Code != tc.want || tc.want == http.StatusSeeOther &&
			!strings.HasPrefix(rr.Header().Get("Location"), "/login?challenge=") {
			t.Errorf("%s: code=%d location=%q, want %d", name, rr.Code, rr.Header().Get("Location"), tc.want)
		}
	}
}

func TestAnonymousApprovalsLeaveRoomForSignedInCallers(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("local-operator", "Local operator", "local")
	browser := func(verifier, remote, cookie string) {
		query := url.Values{"challenge": {challengeFor(verifier)}, "client_origin": {requestingUI}}
		r := requestFrom(http.MethodGet, "/auth/browser?"+query.Encode(), remote)
		if cookie != "" {
			withSessionCookie(r, cookie)
		}
		s.browserPage(httptest.NewRecorder(), r)
	}
	for i := range maxApprovals {
		browser(fmt.Sprint("anonymous-", i), addressFrom(i), "")
	}
	if len(s.approvals) != maxApprovals/2 {
		t.Fatalf("anonymous callers opened %d approvals, want %d", len(s.approvals), maxApprovals/2)
	}
	browser("signed-in-browser", addressFrom(maxApprovals), raw)
	rr := testkit.Record(s.cliPage, cliPageRequest(challengeFor("signed-in-cli"), raw))
	if rr.Code != http.StatusOK || s.approvals[challengeFor("signed-in-browser")] == nil {
		t.Fatalf("signed-in approvals refused: cli %d, browser %v", rr.Code,
			s.approvals[challengeFor("signed-in-browser")] != nil)
	}
}

func TestCliPageRendersApprovalReusesAndCapsIt(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local")
	challenge := challengeFor("verifier-render")
	for range 2 {
		rr := testkit.Record(s.cliPage, cliPageRequest(challenge, raw))
		body := rr.Body.String()
		if rr.Code != http.StatusOK || !strings.Contains(body, verificationCode(challenge)) ||
			!strings.Contains(body, sess.csrf) {
			t.Fatalf("approval page code=%d lacks the verification code or CSRF token", rr.Code)
		}
	}
	if a := s.approvals[challenge]; len(s.approvals) != 1 || a.session != sess || a.approved {
		t.Fatalf("renders left %d approvals, want one pending approval bound to the session", len(s.approvals))
	}
	for i := 1; i < maxSessionApprovals; i++ {
		rr := testkit.Record(s.cliPage, cliPageRequest(challengeFor(fmt.Sprint("verifier-cap-", i)), raw))
		if rr.Code != http.StatusOK {
			t.Fatalf("approval %d code=%d, want 200", i, rr.Code)
		}
	}
	rr := testkit.Record(s.cliPage, cliPageRequest(challengeFor("verifier-cap-over"), raw))
	if rr.Code != http.StatusForbidden {
		t.Fatalf("approval over the per-session cap code=%d, want 403", rr.Code)
	}
}

func TestCliApprove(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local")
	otherRaw, other, _ := s.createSession("local-operator", "Local operator", "local")
	challenge, expired := challengeFor("verifier-approve"), challengeFor("verifier-expired")
	s.cliPage(httptest.NewRecorder(), cliPageRequest(challenge, raw))
	s.cliPage(httptest.NewRecorder(), cliPageRequest(expired, raw))
	s.approvals[expired].expires = time.Now().Add(-time.Second)
	wrongOrigin := approvalForm("/auth/cli/approve", challenge, raw, sess)
	wrongOrigin.Header.Set("Origin", "https://evil.example")
	for name, r := range map[string]*http.Request{
		"another login's csrf": approvalForm("/auth/cli/approve", challenge, raw, other),
		"wrong origin":         wrongOrigin,
		"foreign session":      approvalForm("/auth/cli/approve", challenge, otherRaw, other),
		"unknown challenge":    approvalForm("/auth/cli/approve", challengeFor("nope"), raw, sess),
		"expired approval":     approvalForm("/auth/cli/approve", expired, raw, sess),
	} {
		if w := serveMounted(s, r); w.Code != http.StatusForbidden {
			t.Errorf("%s: code=%d, want 403", name, w.Code)
		}
	}
	if s.approvals[challenge].approved || s.approvals[expired].approved {
		t.Fatal("a rejected request still marked an approval approved")
	}
	w := serveMounted(s, approvalForm("/auth/cli/approve", challenge, raw, sess))
	if w.Code != http.StatusOK || !s.approvals[challenge].approved {
		t.Fatalf("approve code=%d, want 200 and an approved approval", w.Code)
	}
}

func TestCLIExchangeIsSingleUse(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("subject", "Name", "local")
	if rr := cliExchange(s, `{"verifier":"not-known"}`); rr.Code != http.StatusAccepted || len(s.approvals) != 0 {
		t.Fatalf("unknown verifier code=%d approvals=%d, want 202 and no state", rr.Code, len(s.approvals))
	}
	approveNative(t, s, raw, sess, "strict-json-verifier")
	dup := cliExchange(s, `{"verifier":"unknown","verifier":"strict-json-verifier"}`)
	if dup.Code != http.StatusAccepted || len(sess.grants) != 0 {
		t.Fatalf("duplicate-name request code=%d grants=%d, want 202 and no grant", dup.Code, len(sess.grants))
	}
	approveNative(t, s, raw, sess, "terminal-verifier")
	first := cliExchange(s, `{"verifier":"terminal-verifier"}`)
	var out struct {
		Token string `json:"token"`
	}
	if err := json.Unmarshal(first.Body.Bytes(), &out); first.Code != 200 || err != nil || out.Token == "" {
		t.Fatalf("first exchange code=%d body=%s", first.Code, first.Body.String())
	}
	if _, ok := s.authenticateGrant(out.Token); !ok {
		t.Fatal("grant not accepted")
	}
	if replay := cliExchange(s, `{"verifier":"terminal-verifier"}`); replay.Code != http.StatusAccepted {
		t.Fatalf("replay code=%d, want 202", replay.Code)
	}
}

// A CLI login at the grant cap replaces the oldest CLI grant and never a browser grant whose run may be live.
func TestCLIGrantSetIsBoundedWithoutEvictingBrowserGrants(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("subject", "Name", "local")
	var browser []*grant
	addBrowserGrant := func() {
		_, g := addGrant(s, sess, requestingUI)
		browser = append(browser, g)
	}
	exchange := func(i int) int {
		verifier := fmt.Sprintf("verifier-%d", i)
		before := maps.Clone(sess.grants)
		approveNative(t, s, raw, sess, verifier)
		if !maps.Equal(before, sess.grants) {
			t.Fatalf("polling unapproved login %d changed the grant set", i)
		}
		return cliExchange(s, `{"verifier":"`+verifier+`"}`).Code
	}
	addBrowserGrant()
	for i := range 20 {
		if code := exchange(i); code != http.StatusOK {
			t.Fatalf("exchange %d code=%d, want 200", i, code)
		}
	}
	if len(sess.grants) != maxSessionGrants || browser[0].ctx.Err() != nil {
		t.Fatalf("grants=%d browser grant cancelled=%v, want %d grants with the browser grant live",
			len(sess.grants), browser[0].ctx.Err() != nil, maxSessionGrants)
	}
	for _, g := range sess.grants {
		if g.origin == "" {
			s.deleteGrantLocked(g)
		}
	}
	for len(browser) < maxSessionGrants {
		addBrowserGrant()
	}
	if code := exchange(99); code != http.StatusTooManyRequests {
		t.Fatalf("CLI exchange against %d browser grants = %d, want 429", maxSessionGrants, code)
	}
	for i, g := range browser {
		if g.ctx.Err() != nil {
			t.Fatalf("browser grant %d was cancelled by a CLI login", i)
		}
	}
}

// Each approval is confirmed and redeemed only through the flow and audience that created it.
func TestApprovalsStayOnTheirAudience(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("subject", "Name", "local")
	native, browser := "native-audience-verifier", randomToken(32)
	browserPage := func(origin string) *http.Request {
		query := url.Values{"challenge": {challengeFor(browser)}, "client_origin": {origin}}.Encode()
		return withSessionCookie(secureRequest(http.MethodGet, "/auth/browser?"+query, nil), raw)
	}
	if w := serveMounted(s, cliPageRequest(challengeFor(native), raw)); w.Code != http.StatusOK {
		t.Fatalf("terminal page = %d", w.Code)
	}
	if w := serveMounted(s, browserPage(requestingUI)); w.Code != http.StatusOK {
		t.Fatalf("browser page = %d", w.Code)
	}
	for name, r := range map[string]*http.Request{
		"terminal approval confirmed as a browser": approvalForm("/auth/browser/approve", challengeFor(native), raw,
			sess),
		"browser approval confirmed as a terminal": approvalForm("/auth/cli/approve", challengeFor(browser), raw,
			sess),
		"browser approval shown for another origin": browserPage("https://other.example"),
	} {
		if w := serveMounted(s, r); w.Code != http.StatusForbidden {
			t.Errorf("%s = %d, want 403", name, w.Code)
		}
	}
	if s.approvals[challengeFor(native)].approved || s.approvals[challengeFor(browser)].approved {
		t.Fatal("a cross-audience confirmation approved a request")
	}
	for path, challenge := range map[string]string{"/auth/cli/approve": challengeFor(native),
		"/auth/browser/approve": challengeFor(browser)} {
		if w := serveMounted(s, approvalForm(path, challenge, raw, sess)); w.Code != http.StatusOK {
			t.Fatalf("%s = %d, want 200", path, w.Code)
		}
	}
	if w := cliExchange(s, `{"verifier":"`+browser+`"}`); w.Code != http.StatusAccepted {
		t.Fatalf("browser approval redeemed by a terminal = %d, want 202", w.Code)
	}
	if w := serveMounted(s, browserExchangeRequest(native, requestingUI)); w.Code == http.StatusOK {
		t.Fatal("terminal approval redeemed by a browser")
	}
	if len(sess.grants) != 0 {
		t.Fatalf("cross-audience exchanges issued %d grants", len(sess.grants))
	}
	if w := serveMounted(s, browserExchangeRequest(browser, requestingUI)); w.Code != http.StatusOK {
		t.Fatalf("browser exchange on its audience = %d, want 200", w.Code)
	}
	if w := cliExchange(s, `{"verifier":"`+native+`"}`); w.Code != http.StatusOK {
		t.Fatalf("terminal exchange = %d, want 200", w.Code)
	}
}

// Signing out ends the login's pending approvals, so signing back in can confirm the same terminal request.
func TestReloginConfirmsAPendingTerminalApproval(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local")
	const verifier = "relogin-terminal-verifier"
	if w := serveMounted(s, cliPageRequest(challengeFor(verifier), raw)); w.Code != http.StatusOK {
		t.Fatalf("terminal page = %d", w.Code)
	}
	if w := serveMounted(s, approvalForm("/auth/logout", "", raw, sess)); w.Code != http.StatusSeeOther {
		t.Fatalf("logout = %d", w.Code)
	}
	raw, sess, _ = s.createSession("local-operator", "Local operator", "local")
	if _, ok := s.authenticateGrant(nativeGrant(t, s, raw, sess, verifier)); !ok {
		t.Fatal("the grant confirmed after signing back in was refused")
	}
}

var markup = regexp.MustCompile(`<[^>]*>`)

// refusalCard asserts an approval page refusal: a 403 whose card is one fixed sentence and echoes nothing.
func refusalCard(t *testing.T, name string, rr *httptest.ResponseRecorder, sentence string, echoes ...string) {
	t.Helper()
	body := rr.Body.String()
	_, card, _ := strings.Cut(body, "Graphite Meter</p>")
	card, _, _ = strings.Cut(card, "</main>")
	text := strings.Join(strings.Fields(markup.ReplaceAllString(card, " ")), " ")
	if rr.Code != http.StatusForbidden || !strings.HasPrefix(rr.Header().Get("Content-Type"), "text/html") ||
		text != "Approval unavailable "+sentence || strings.Contains(card, "<form") {
		t.Errorf("%s: %d %q, want a 403 card saying only %q", name, rr.Code, text, sentence)
	}
	for _, echo := range echoes {
		if strings.Contains(body, echo) {
			t.Errorf("%s: the refusal echoes %q", name, echo)
		}
	}
}

func browserPageRequest(challenge, origin, remote, cookie string) *http.Request {
	r := requestFrom(http.MethodGet, "/auth/browser?"+url.Values{"challenge": {challenge},
		"client_origin": {origin}}.Encode(), remote)
	if cookie != "" {
		withSessionCookie(r, cookie)
	}
	return r
}

const refusedLinkSentence = "This approval link is not valid. Start sign-in again from the client."

func TestApprovalPagesRefuseALinkTheyCannotApprove(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("local-operator", "Local operator", "local")
	otherRaw, _, _ := s.createSession("local-operator", "Local operator", "local")
	const remote = "198.51.100.7:40000"
	bound, elsewhere := challengeFor("bound-to-a-login"), challengeFor("opened-for-another-site")
	testkit.Record(s.browserPage, browserPageRequest(bound, requestingUI, remote, raw))
	testkit.Record(s.browserPage, browserPageRequest(elsewhere, requestingUI, remote, ""))
	for name, tc := range map[string]struct {
		page   http.HandlerFunc
		r      *http.Request
		echoes []string
	}{
		"terminal link without a challenge": {s.cliPage, cliPageRequest("not-a-challenge", raw),
			[]string{"not-a-challenge"}},
		"browser link without a challenge": {s.browserPage,
			browserPageRequest("not-a-challenge", requestingUI, remote, raw), []string{"not-a-challenge", requestingUI}},
		"clear audience": {s.browserPage, browserPageRequest(bound, "http://console.example", remote, raw),
			[]string{bound, "console.example"}},
		"another site's approval": {s.browserPage, browserPageRequest(elsewhere, "https://other.example", remote, raw),
			[]string{elsewhere, "other.example", "console.example"}},
		"another login's approval": {s.browserPage, browserPageRequest(bound, requestingUI, remote, otherRaw),
			[]string{bound, "console.example"}},
	} {
		refusalCard(t, name, testkit.Record(tc.page, tc.r), refusedLinkSentence, tc.echoes...)
	}
}

func TestApprovalPagesRefuseSpentBudgetsWithoutCounts(t *testing.T) {
	const sentence = "Too many approvals are open. Try again from the client in a few minutes."
	t.Run("terminal approvals of one login", func(t *testing.T) {
		s := testService(t)
		raw, _, _ := s.createSession("local-operator", "Local operator", "local")
		for i := range maxSessionApprovals {
			testkit.Record(s.cliPage, cliPageRequest(challengeFor(fmt.Sprint("open-", i)), raw))
		}
		over := challengeFor("one-too-many")
		refusalCard(t, "cli", testkit.Record(s.cliPage, cliPageRequest(over, raw)), sentence, over)
	})
	t.Run("an ambiguous client", func(t *testing.T) {
		s := proxiedService(t)
		raw, _, _ := s.createSession("local-operator", "Local operator", "local")
		challenge := challengeFor("unattributable")
		r := withSessionCookie(requestFrom(http.MethodGet, "/auth/cli?challenge="+challenge, "192.0.2.10:40000"), raw)
		refusalCard(t, "cli", testkit.Record(s.cliPage, r), sentence, challenge)
		r = browserPageRequest(challenge, requestingUI, "192.0.2.10:40000", raw)
		refusalCard(t, "browser", testkit.Record(s.browserPage, r), sentence, challenge, "console.example")
	})
	t.Run("browser approval pages of one address", func(t *testing.T) {
		s := testService(t)
		const remote = "198.51.100.8:40000"
		refused := 0
		for i := range maxAddressApprovals + 1 {
			challenge := challengeFor(fmt.Sprint("page-", i))
			rr := testkit.Record(s.browserPage, browserPageRequest(challenge, requestingUI, remote, ""))
			if rr.Code == http.StatusForbidden {
				refused++
				refusalCard(t, fmt.Sprint("page ", i), rr, sentence, challenge, "console.example")
			}
		}
		if refused != maxAddressApprovals+1-maxClientApprovals {
			t.Fatalf("refused %d approval pages", refused)
		}
	})
	t.Run("browser approvals of one login", func(t *testing.T) {
		s := testService(t)
		raw, _, _ := s.createSession("local-operator", "Local operator", "local")
		for i := range maxSessionApprovals {
			testkit.Record(s.browserPage, browserPageRequest(challengeFor(fmt.Sprint("bound-", i)), requestingUI,
				addressFrom(i), raw))
		}
		over := challengeFor("unbound")
		rr := testkit.Record(s.browserPage, browserPageRequest(over, requestingUI, addressFrom(99), raw))
		refusalCard(t, "browser", rr, sentence, over, "console.example")
	})
}
