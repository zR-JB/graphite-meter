package goclient

import (
	"crypto/tls"
	"encoding/json/v2"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"sync"
	"testing"
	"time"

	"github.com/quic-go/quic-go/http3"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/apipin"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestUploadRefusalCodesActTheSameOnEveryTransport(t *testing.T) {
	t.Parallel()
	want := map[string]FailureReason{"invalid": FailureConnectionLost, "globalFull": FailureServerBusy,
		"clientFull": FailureServerBusy, "ownerMismatch": FailureProtocol, "idle": FailureTimeout,
		"revoked": FailureSignIn}
	retried := map[string]bool{"globalFull": true, "clientFull": true, "idle": true}
	for _, fields := range apipin.Rows(t, "uploadrefusals.txt", 3) {
		code := fields[0]
		status, _ := strconv.Atoi(fields[2])
		header := http.Header{"X-Graphite-Upload-Refusal": {code}}
		for transport, err := range map[string]error{
			"HTTP":         statusOf(&http.Response{StatusCode: status, Header: header}, "fixture"),
			"WebTransport": uploadRefusal(code, statusError{from: "fixture"}),
		} {
			if reason := failureReason(err, false); reason != want[code] || permanent(err) == retried[code] {
				t.Errorf("%s over %s = %v (%s, permanent %v), want %s", code, transport, err, reason,
					permanent(err), want[code])
			}
		}
		delete(want, code)
	}
	if len(want) > 0 {
		t.Errorf("the refusal pin no longer lists %v", want)
	}
}

func TestSetupBusyRefusalsRetryOnEveryTransport(t *testing.T) {
	t.Parallel()
	honoured := []time.Duration{time.Second}
	doubling := []time.Duration{busyBackoff, 2 * busyBackoff}
	for _, c := range []struct {
		name, protocol, transport, refused string
		record                             bool
		gaps                               []time.Duration
	}{
		{"upload session over HTTP/1.1", "http1", wire.TransportFetchStream, route.UploadSession, false, honoured},
		{"upload feed over HTTP/1.1", "http1", wire.TransportFetchStream, route.UploadProgress, false, honoured},
		{"upload feed over HTTP/2", "http2", wire.TransportFetchStream, route.UploadProgress, false, honoured},
		{"upload feed over HTTP/3", "http3", wire.TransportFetchStream, route.UploadProgress, false, honoured},
		{"WebTransport session", "http3", wire.TransportWebTransport, route.WTUpload, false, honoured},
		{"WebTransport refusal record", "http3", wire.TransportWebTransport, route.WTUpload, true, doubling},
		{"WebSocket latency", "http1", wire.TransportFetchStream, route.Ping, false, honoured},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			var mu sync.Mutex
			var arrivals []time.Time
			origin := testOrigin(t, c.protocol, func(wt *webtransport.Server) http.Handler {
				return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					if r.URL.Path != c.refused || r.Method == http.MethodDelete {
						_ = json.MarshalWrite(w, wire.UploadSession{UploadID: "busy"})
						return
					}
					mu.Lock()
					arrivals = append(arrivals, time.Now())
					mu.Unlock()
					if c.record {
						refuseInRecord(wt, w, r)
						return
					}
					w.Header().Set("Retry-After", "1")
					w.WriteHeader(http.StatusTooManyRequests)
				})
			})
			cred := credential{insecure: true}
			r := &runner{cfg: DefaultConfig().normalized(), cred: cred, coordinated: &participantCounters{},
				target:        &wire.ThroughputTarget{Origin: origin, Transport: c.transport, Protocol: c.protocol},
				latencyTarget: &wire.LatencyTarget{Origin: origin, Transport: wire.TransportWebSocket},
				streams:       byDirection[int]{1, 1}, teardown: t.Context(), emit: func(Event) {}}
			var closeHTTP, closeWS func()
			r.http, closeHTTP = protocolClient(cred, c.protocol)
			r.websocketHTTP, closeWS = websocketClient(cred)
			defer closeHTTP()
			defer closeWS()
			gate := testStageGate(make(chan struct{}))
			var err error
			if c.refused == route.Ping {
				_, err = r.measureLatency(t.Context(), StageLatency, false, time.Second, gate)
			} else {
				err = r.measureUpload(t.Context(), gate)
			}
			mu.Lock()
			defer mu.Unlock()
			if reason := failureReason(err, true); reason != FailureServerBusy || len(arrivals) <= len(c.gaps) {
				t.Fatalf("%v (%s) after %d attempts, want server-busy after retries", err, reason, len(arrivals))
			}
			for i, least := range c.gaps {
				if gap := arrivals[i+1].Sub(arrivals[i]); gap < least {
					t.Errorf("retry %d came after %v, want at least %v", i+1, gap, least)
				}
			}
		})
	}
}

func refuseInRecord(wt *webtransport.Server, w http.ResponseWriter, r *http.Request) {
	sess, err := wt.Upgrade(w, r)
	if err != nil {
		return
	}
	if str, err := sess.OpenUniStreamSync(r.Context()); err == nil {
		_, _ = io.WriteString(str, `{"type":"error","code":"clientFull","message":"client upload capacity exhausted"}`+"\n")
		_ = str.Close()
	}
	select {
	case <-sess.Context().Done():
	case <-time.After(redialWindow):
	}
}

// testOrigin serves handler over protocol; HTTP/3 also accepts WebTransport sessions.
func testOrigin(t *testing.T, protocol string, handler func(*webtransport.Server) http.Handler) string {
	t.Helper()
	if protocol != "http3" {
		srv := httptest.NewUnstartedServer(handler(nil))
		if srv.EnableHTTP2 = protocol == "http2"; srv.EnableHTTP2 {
			srv.StartTLS()
		} else {
			srv.Start()
		}
		t.Cleanup(srv.Close)
		return srv.URL
	}
	certificates := httptest.NewTLSServer(http.NotFoundHandler())
	certificates.Close()
	h3 := &http3.Server{
		TLSConfig: &tls.Config{Certificates: certificates.TLS.Certificates, NextProtos: []string{http3.NextProtoH3}},
	}
	webtransport.ConfigureHTTP3Server(h3)
	server := &webtransport.Server{H3: h3}
	h3.Handler = handler(server)
	conn, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	go func() { _ = server.Serve(conn) }()
	t.Cleanup(func() { _ = server.Close() })
	return "https://" + conn.LocalAddr().String()
}
