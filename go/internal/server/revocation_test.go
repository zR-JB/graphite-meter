package server

import (
	"bufio"
	"crypto/tls"
	"encoding/json/v2"
	"io"
	"net/http"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// signOut ends the stack's login, and with it every grant the login approved.
func (s *authenticatedStack) signOut(t *testing.T) {
	t.Helper()
	form := url.Values{"csrf": {s.csrf.Value}}.Encode()
	req, _ := http.NewRequest(http.MethodPost, s.origin+"/auth/logout", strings.NewReader(form))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Origin", s.origin)
	req.AddCookie(s.session)
	res, err := s.uiClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
}

func (s *authenticatedStack) mintUpload(t *testing.T, bearer string) string {
	t.Helper()
	req, _ := http.NewRequest(http.MethodPost, s.origin+"/upload/session", nil)
	req.Header.Set("Authorization", "Bearer "+bearer)
	res, err := s.uiClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	var minted struct {
		UploadID string `json:"uploadId"`
	}
	if err := json.UnmarshalRead(res.Body, &minted); err != nil || minted.UploadID == "" {
		t.Fatalf("upload session = %d: %v", res.StatusCode, err)
	}
	return minted.UploadID
}

// awaitAdmitted waits until the probe counts the lane in flight, so sign-out finds it reading.
func (s *authenticatedStack) awaitAdmitted(t *testing.T, bearer string) {
	t.Helper()
	for start := time.Now(); time.Since(start) < 5*time.Second; time.Sleep(time.Millisecond) {
		req, _ := http.NewRequest(http.MethodGet, s.origin+"/probe", nil)
		req.Header.Set("Authorization", "Bearer "+bearer)
		res, err := s.uiClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		var probe wire.Probe
		err = json.UnmarshalRead(res.Body, &probe)
		res.Body.Close()
		if err == nil && probe.Load != nil && probe.Load.Active > 0 {
			return
		}
	}
	t.Fatal("the upload lane was never admitted")
}

// Signing out ends an upload lane blocked on its body at once, with the answer both clients read as sign-in.
func TestSignOutEndsAStalledHTTPUploadLane(t *testing.T) {
	t.Parallel()
	for _, protocol := range []string{"http1", "http2"} {
		t.Run(protocol, func(t *testing.T) {
			t.Parallel()
			s := newAuthenticatedStack(t)
			bearer := s.grant(t)
			id := s.mintUpload(t, bearer)
			answered := make(chan *http.Response, 1)
			if protocol == "http1" {
				conn, err := tls.Dial("tcp", strings.TrimPrefix(s.origin, "https://"),
					&tls.Config{InsecureSkipVerify: true, NextProtos: []string{"http/1.1"}}) //nolint:gosec
				if err != nil {
					t.Fatal(err)
				}
				defer conn.Close()
				head := "POST /upload?id=" + id + " HTTP/1.1\r\nHost: " + strings.TrimPrefix(s.origin, "https://") +
					"\r\nAuthorization: Bearer " + bearer + "\r\nContent-Length: 1073741824\r\n\r\n"
				if _, err := io.WriteString(conn, head+strings.Repeat("x", 64<<10)); err != nil {
					t.Fatal(err)
				}
				go func() {
					res, _ := http.ReadResponse(bufio.NewReader(conn), nil)
					answered <- res
				}()
			} else {
				body, w := io.Pipe()
				defer w.Close()
				req, _ := http.NewRequest(http.MethodPost, s.h2URL+"/upload?id="+id, body)
				req.Header.Set("Authorization", "Bearer "+bearer)
				go func() {
					res, _ := s.h2Client.Do(req)
					answered <- res
				}()
				if _, err := w.Write(make([]byte, 64<<10)); err != nil {
					t.Fatal(err)
				}
			}
			s.awaitAdmitted(t, bearer)
			s.signOut(t)
			revoked := time.Now()
			select {
			case res := <-answered:
				if took := time.Since(revoked); res == nil || res.StatusCode != http.StatusForbidden ||
					res.Header.Get("Graphite-Meter-Auth") != "required" ||
					res.Header.Get("X-Graphite-Upload-Refusal") != "revoked" || took > 250*time.Millisecond {
					t.Fatalf("revoked lane answered %v after %v", res, took)
				}
				res.Body.Close()
			case <-time.After(5 * time.Second):
				t.Fatal("a signed-out upload lane kept reading")
			}
		})
	}
}
