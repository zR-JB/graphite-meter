package goclient

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/quic-go/quic-go/http3"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type PreparedConnection struct {
	Preflight        wire.Preflight
	ThroughputTarget wire.ThroughputTarget
	LatencyTarget    *wire.LatencyTarget
	Probe            wire.Probe
	LatencyProbe     *wire.Probe
	VerifiedAt       time.Time
	configKey        string
}

// PreparationError retains successful discovery when a later target check
// fails, so interactive clients can offer the advertised alternatives.
type PreparationError struct {
	Preflight wire.Preflight
	Err       error
}

func (e *PreparationError) Error() string { return e.Err.Error() }
func (e *PreparationError) Unwrap() error { return e.Err }

const preparationFreshness = 30 * time.Second

// preparationKey covers every field Prepare either validates or discovers
// against, so a change to one of them re-prepares instead of reusing a
// PreparedConnection that was never checked for it. PingInterval is here for the
// validation alone: nothing about it is discovered, but ValidatePingInterval
// runs only in Prepare, and RunPrepared would otherwise take a cadence the
// server's idle bound reaps the bus at.
func preparationKey(cfg Config) string {
	return fmt.Sprintf("%s\n%s\n%s\n%s\n%s\n%s\n%s\n%t\n%t\n%t", cfg.BaseURL, cfg.ThroughputTarget, cfg.ThroughputProtocol, cfg.ThroughputTransport, cfg.LatencyTarget, cfg.LatencyTransport, cfg.PingInterval, cfg.InsecureSkipTLSVerify, cfg.needsLatency(), cfg.authToken() != "")
}

// needsLatency reports whether a latency target has to be discovered and
// verified: either the latency stage runs on its own, or a transfer stage
// carries a loaded-latency probe alongside it.
func (c Config) needsLatency() bool {
	return c.Stages.Latency || (c.LoadedLatency && (c.Stages.Download || c.Stages.Upload || c.Stages.Bidirectional))
}

func canonicalOrigin(raw string) (string, error) {
	u, err := url.Parse(raw)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" || u.User != nil || (u.Path != "" && u.Path != "/") || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" {
		return "", errors.New("invalid server URL")
	}
	return strings.ToLower(u.Scheme) + "://" + strings.ToLower(u.Host), nil
}

func CanonicalServerOrigin(raw string) (string, error) { return canonicalOrigin(raw) }

func (c Config) authToken() string {
	serverOrigin, err := canonicalOrigin(c.BaseURL)
	if err != nil || !strings.EqualFold(serverOrigin, c.AuthOrigin) {
		return ""
	}
	return c.AuthToken
}

type authTransport struct {
	token, hostname string
	base            http.RoundTripper
}

func (t authTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	if t.token == "" {
		return t.base.RoundTrip(r)
	}
	if r.URL.Scheme != "https" || !strings.EqualFold(r.URL.Hostname(), t.hostname) {
		return nil, fmt.Errorf("refusing to send authentication grant outside canonical HTTPS host")
	}
	clone := r.Clone(r.Context())
	clone.Header = r.Header.Clone()
	clone.Header.Set("Authorization", "Bearer "+t.token)
	return t.base.RoundTrip(clone)
}

// pinnedHostname is empty for an origin that does not parse, which makes
// authTransport refuse every request rather than send the grant to a host it
// cannot name.
func pinnedHostname(origin string) string {
	u, err := url.Parse(origin)
	if err != nil {
		return ""
	}
	return u.Hostname()
}

func authenticatedClient(cfg Config, base http.RoundTripper) *http.Client {
	token := cfg.authToken()
	client := &http.Client{Transport: authTransport{token: token, hostname: pinnedHostname(cfg.AuthOrigin), base: base}}
	if token != "" {
		client.CheckRedirect = func(*http.Request, []*http.Request) error {
			return errors.New("authenticated measurement endpoints must not redirect")
		}
	}
	return client
}

func (p *PreparedConnection) FreshFor(cfg Config) bool {
	return p != nil && p.configKey == preparationKey(cfg.normalized()) && time.Since(p.VerifiedAt) <= preparationFreshness
}

// ConnectionSummary names one measurement path: the mechanism that carries it,
// the HTTP version underneath, and whether it is encrypted. Every surface that
// names a path uses it — the TUI's selectors, its readiness panel, and its run
// screen — so a path reads the same wherever it is mentioned. The protocol is
// accepted in either spelling, since a run holds the negotiated evidence ("h3")
// where discovery holds the target's own name for it ("http3").
func ConnectionSummary(transport, protocol string, tls bool) string {
	mechanism := map[string]string{
		wire.TransportWebSocket:            "WebSocket",
		wire.TransportWebTransport:         "WebTransport",
		wire.TransportWebTransportDatagram: "WebTransport datagrams",
	}[transport]
	if mechanism == "" {
		mechanism = "Fetch stream"
	}
	security := "clear"
	if tls {
		security = "TLS"
	}
	return fmt.Sprintf("%s · %s · %s", mechanism, ProtocolLabel(protocol), security)
}

// ProtocolLabel names an HTTP version for a reader, in either spelling: the
// negotiated evidence a run holds ("h3") and the name discovery gives the same
// version ("http3") are one line on screen, not two. An unnamed version renders
// as a dash rather than as nothing, which in the middle of a summary would be
// two separators with a gap between them.
func ProtocolLabel(protocol string) string {
	switch protocolFromEvidence(protocol) {
	case "http1":
		return "HTTP/1.1"
	case "http2":
		return "HTTP/2"
	case "http3":
		return "HTTP/3"
	// A negotiated origin fixes no version: the transport picks one at connect
	// time, and until then that is the honest thing to name.
	case "negotiated":
		return "Negotiated"
	case "":
		return "--"
	}
	return protocol
}

func (p *PreparedConnection) ThroughputSummary() string {
	if p == nil {
		return "Not checked"
	}
	t := p.ThroughputTarget
	return ConnectionSummary(t.Transport, t.Protocol, t.TLS)
}

func (p *PreparedConnection) LatencySummary() string {
	if p == nil || p.LatencyTarget == nil {
		return "Not selected"
	}
	t := p.LatencyTarget
	return ConnectionSummary(t.Transport, t.Protocol, t.TLS)
}

func baseTransport(cfg Config) *http.Transport {
	return &http.Transport{
		Proxy:                 http.ProxyFromEnvironment,
		DialContext:           (&net.Dialer{Timeout: 10 * time.Second, KeepAlive: 30 * time.Second}).DialContext,
		ForceAttemptHTTP2:     true,
		MaxIdleConns:          cfg.MaxIdleConnsPerHost * 2,
		MaxIdleConnsPerHost:   cfg.MaxIdleConnsPerHost,
		MaxConnsPerHost:       0,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: cfg.ResponseHeaderTimeout,
		ExpectContinueTimeout: cfg.ExpectContinueTimeout,
		TLSClientConfig:       &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
		WriteBufferSize:       256 * 1024,
		ReadBufferSize:        256 * 1024,
	}
}

// websocketClient pins HTTP/1.1: the latency channel is an HTTP/1 Upgrade
// handshake, which an HTTP/2 or HTTP/3 connection cannot carry.
func websocketClient(cfg Config) (*http.Client, func()) {
	tr := baseTransport(cfg)
	protocols := &http.Protocols{}
	protocols.SetHTTP1(true)
	tr.Protocols = protocols
	return authenticatedClient(cfg, tr), tr.CloseIdleConnections
}

func Prepare(ctx context.Context, cfg Config) (*PreparedConnection, error) {
	cfg = cfg.normalized()
	if cfg.authToken() != "" {
		u, err := url.Parse(cfg.BaseURL)
		if err != nil || u.Scheme != "https" || cfg.InsecureSkipTLSVerify {
			return nil, fmt.Errorf("authenticated operation requires verified HTTPS -url")
		}
	}
	switch cfg.ThroughputProtocol {
	case "auto", "http1", "http2", "http3":
	default:
		return nil, fmt.Errorf("invalid throughput protocol %q", cfg.ThroughputProtocol)
	}
	// A misspelled transport is answerable without a server, and the selection
	// failure it otherwise produces ("auto target unavailable over webscoket")
	// names the typo only to a reader who already knows the spelling.
	if err := ValidateThroughputTransport(cfg.ThroughputTransport); err != nil {
		return nil, err
	}
	if err := ValidateLatencyTransport(cfg.LatencyTransport); err != nil {
		return nil, err
	}
	discoveryTransport := baseTransport(cfg)
	defer discoveryTransport.CloseIdleConnections()
	discoveryClient := authenticatedClient(cfg, discoveryTransport)

	pf, err := getPreflight(ctx, discoveryClient, cfg.BaseURL)
	if err != nil {
		return nil, err
	}
	fail := func(err error) (*PreparedConnection, error) {
		return nil, &PreparationError{Preflight: pf, Err: err}
	}
	advertisedTarget, err := selectTarget(cfg, pf)
	if err != nil {
		return fail(err)
	}
	// An advertised WebTransport target still needs UDP to reach the server, and
	// a blocked path surfaces here as a QUIC dial error. Automatic selection
	// reaches WebTransport whenever fetch selection failed -- including when it
	// refused an ambiguous choice between several advertised fetch origins --
	// so under auto the fetch pass is re-run and its own refusal, which is the
	// one an operator can act on, is what the caller sees.
	if advertisedTarget.Transport == wire.TransportWebTransport {
		if verifyErr := verifyThroughputWebTransport(ctx, cfg, advertisedTarget); verifyErr != nil {
			if cfg.ThroughputTransport != "auto" {
				return fail(verifyErr)
			}
			fetchTarget, fetchErr := selectTargetOver(cfg, pf, wire.TransportFetchStream)
			if fetchErr != nil {
				return fail(fmt.Errorf("%w (the advertised WebTransport target is unreachable: %v)", fetchErr, verifyErr))
			}
			advertisedTarget = fetchTarget
		}
	}
	target := *advertisedTarget
	if cfg.ThroughputProtocol != "auto" {
		if target.Protocol != "negotiated" && target.Protocol != cfg.ThroughputProtocol {
			return fail(fmt.Errorf("endpoint is fixed to %s, cannot use %s", target.Protocol, cfg.ThroughputProtocol))
		}
		target.Protocol = cfg.ThroughputProtocol
	}
	transfer, closeTransfer := protocolClient(cfg, target.Protocol, func() *http.Transport { return baseTransport(cfg) })
	defer closeTransfer()
	probe, clientProtocol, err := getProbe(ctx, transfer, &target)
	if err != nil {
		return fail(err)
	}
	if target.Protocol == "negotiated" {
		target.Protocol = protocolFromEvidence(clientProtocol)
	}
	wsClient, closeWebSocket := websocketClient(cfg)
	defer closeWebSocket()
	latencyTarget, latencyErr := selectLatencyTarget(cfg, pf.Capabilities.LatencyTargets)
	needsLatency := cfg.needsLatency()
	if latencyErr != nil && needsLatency {
		return fail(latencyErr)
	}
	var latencyProbe *wire.Probe
	if !needsLatency {
		latencyTarget = nil
	} else if latencyTarget != nil {
		// An explicitly selected datagram bus is already final, so reject its
		// invalid cadence before spending time on network verification. Automatic
		// selection has to verify/fall back before the same decision is possible.
		if cfg.LatencyTransport != "auto" && PingIntervalBoundApplies(latencyTarget.Transport) {
			if err := ValidatePingInterval(cfg.PingInterval); err != nil {
				return fail(err)
			}
		}
		if latencyTarget.Transport == wire.TransportWebTransport {
			if verifyErr := verifyLatencyWebTransport(ctx, cfg, latencyTarget); verifyErr != nil {
				if cfg.LatencyTransport != "auto" {
					return fail(verifyErr)
				}
				if latencyTarget, latencyErr = selectLatencyTargetOver(cfg.LatencyTarget, cfg.BaseURL, pf.Capabilities.LatencyTargets, wire.TransportWebSocket); latencyErr != nil {
					return fail(latencyErr)
				}
			}
		}
		// The idle bound belongs only to the bus Prepare finally commits to.
		// Automatic WebTransport may have fallen back to WebSocket above, whose
		// connection has no corresponding cadence ceiling.
		if cfg.LatencyTransport == "auto" && PingIntervalBoundApplies(latencyTarget.Transport) {
			if err := ValidatePingInterval(cfg.PingInterval); err != nil {
				return fail(err)
			}
		}
		p, err := getLatencyProbe(ctx, wsClient, latencyTarget)
		if err != nil {
			return fail(err)
		}
		latencyProbe = &p
		if latencyTarget.Transport == wire.TransportWebSocket {
			if err := verifyLatencyWebSocket(ctx, wsClient, latencyTarget); err != nil {
				return fail(err)
			}
		}
	}
	return &PreparedConnection{Preflight: pf, ThroughputTarget: target, LatencyTarget: latencyTarget, Probe: probe, LatencyProbe: latencyProbe, VerifiedAt: time.Now(), configKey: preparationKey(cfg)}, nil
}

func Run(ctx context.Context, cfg Config, emit func(Event)) error {
	prepared, err := Prepare(ctx, cfg)
	if err != nil {
		emit(Event{Kind: EventError, At: time.Now(), Err: err})
		return err
	}
	return RunPrepared(ctx, cfg, prepared, emit)
}

func RunPrepared(ctx context.Context, cfg Config, prepared *PreparedConnection, emit func(Event)) error {
	cfg = cfg.normalized()
	if prepared == nil || prepared.configKey != preparationKey(cfg) || time.Since(prepared.VerifiedAt) > preparationFreshness {
		return Run(ctx, cfg, emit)
	}
	target := &prepared.ThroughputTarget
	transfer, closeTransfer := protocolClient(cfg, target.Protocol, func() *http.Transport { return baseTransport(cfg) })
	defer closeTransfer()
	wsClient, closeWebSocket := websocketClient(cfg)
	defer closeWebSocket()
	pf := prepared.Preflight
	probe := prepared.Probe
	latencyTarget := prepared.LatencyTarget
	latencyProbe := prepared.LatencyProbe
	throughputProtocol := targetProtocolEvidence(target.Protocol)
	if target.Protocol == "negotiated" {
		throughputProtocol = probe.ProtocolNegotiated
	}
	event := Event{Kind: EventPreflight, At: time.Now(), Preflight: &pf, Probe: &probe, LatencyProbe: latencyProbe, Message: target.ID, ThroughputTarget: target.ID, ThroughputProtocol: throughputProtocol, ThroughputTransport: target.Transport}
	if latencyTarget != nil {
		event.LatencyTarget = latencyTarget.ID
		event.LatencyTransport = latencyTarget.Transport
		event.LatencyProtocol = latencyBusEvidence(latencyTarget, latencyProbe)
	}
	emit(event)

	r := runner{
		cfg: cfg, streams: cfg.TransferStreams.lanes(target.Protocol, target.Transport),
		http: transfer, websocketHTTP: wsClient,
		target: target, latencyTarget: latencyTarget,
		emit: emit,
	}
	if cfg.Stages.Latency {
		if err := r.runLatencyStage(ctx, "latency", false, cfg.LatencyDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Download {
		if err := r.runTransferStage(ctx, "download", []Direction{Down}, cfg.DownloadDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Upload {
		if err := r.runTransferStage(ctx, "upload", []Direction{Up}, cfg.UploadDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Bidirectional {
		if err := r.runTransferStage(ctx, "bidirectional", []Direction{Down, Up}, cfg.BidirectionalDuration); err != nil {
			return r.fail(err)
		}
	}
	emit(Event{Kind: EventComplete, At: time.Now(), Message: "complete"})
	return nil
}

type runner struct {
	cfg           Config
	streams       streamCounts
	http          *http.Client
	websocketHTTP *http.Client
	target        *wire.ThroughputTarget
	latencyTarget *wire.LatencyTarget
	emit          func(Event)
	// Idle RTT from the latency stage, stretching later stages' warmup so TCP
	// slow-start fills the BDP (0 until measured).
	idleRTT time.Duration
}

// laneStagger spreads lane starts so their congestion windows don't ramp in
// lockstep: synchronised overshoot means synchronised loss and backoff.
const laneStagger = 75 * time.Millisecond

// adaptiveWarmup stretches a stage's warmup to ~10 RTTs so slow-start finishes
// while the measured window is still closed. The configured value is the floor
// and ceil the cap; rtt <= 0 (latency stage disabled or unrun) takes the floor.
func adaptiveWarmup(base, rtt time.Duration) time.Duration {
	const slowStartRTTs = 10
	const ceil = 4 * time.Second
	w := min(max(slowStartRTTs*rtt, base), ceil)
	return w
}

// laneStaggerStep is the per-lane spawn delay for a direction running `streams`
// lanes. Even the last lane (of up to 128) spawns within half the warmup
// window; laneStagger is only the cap. One lane or no warmup gives 0, so every
// lane spawns together.
func (r *runner) laneStaggerStep(streams int) time.Duration {
	if streams <= 1 {
		return 0
	}
	step := min(adaptiveWarmup(r.cfg.Warmup, r.idleRTT)/2/time.Duration(streams-1), laneStagger)
	return step
}

// staggerSleep delays lane `lane` by lane*step (lane 0 / step 0 are immediate),
// returning false if the context is cancelled during the wait.
func staggerSleep(ctx context.Context, lane int, step time.Duration) bool {
	delay := time.Duration(lane) * step
	if delay <= 0 {
		return true
	}
	t := time.NewTimer(delay)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-t.C:
		return true
	}
}

func (r *runner) fail(err error) error {
	if err == nil || errors.Is(err, context.Canceled) {
		return err
	}
	r.emit(Event{Kind: EventError, At: time.Now(), Err: err})
	return err
}

func (r *runner) runLatencyStage(ctx context.Context, stage string, underLoad bool, duration time.Duration) error {
	start := r.warmupGate(ctx, stage)
	stats, err := r.measureLatency(ctx, stage, underLoad, duration, start)
	if err != nil {
		return err
	}
	// Capture idle RTT so the later throughput stages' warmup can scale to it.
	if !underLoad && stats.P50 > 0 {
		r.idleRTT = stats.P50
	}
	res := Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: duration}
	r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: &res})
	return nil
}

// stageFailed reports whether err ends the stage rather than merely stopping
// it. Cancellation is a stop: the caller's, or the sibling direction's through
// cancelStage. Everything else is a failure -- including a bound inside the
// stage expiring, which wraps context.DeadlineExceeded and was once swallowed
// along with it, publishing a part-window byte total as the window's rate.
func stageFailed(err error) bool {
	return err != nil && !errors.Is(err, context.Canceled)
}

func (r *runner) runTransferStage(ctx context.Context, stage string, dirs []Direction, duration time.Duration) error {
	start := r.warmupGate(ctx, stage)

	stageCtx, cancelStage := context.WithCancel(ctx)
	defer cancelStage()

	var wg sync.WaitGroup
	errs := make(chan error, len(dirs)+1)
	results := make(chan Result, len(dirs))
	for _, dir := range dirs {
		wg.Go(func() {
			var res Result
			var err error
			if dir == Down {
				res, err = r.measureDownload(stageCtx, stage, duration, start)
			} else {
				res, err = r.measureUpload(stageCtx, stage, duration, start)
			}
			if stageFailed(err) {
				errs <- err
				return
			}
			results <- res
		})
	}

	// The loaded-latency result is handed back rather than emitted here: the
	// probe treats the stage's cancellation as a clean end, so a stage that
	// failed would still publish a result under its own name and a consumer
	// keyed by stage would read the failed stage as having measured.
	latency := make(chan Result, 1)
	if r.cfg.LoadedLatency {
		wg.Go(func() {
			stats, err := r.measureLatency(stageCtx, stage, true, duration, start)
			if stageFailed(err) {
				errs <- err
				return
			}
			if stats.Count > 0 {
				latency <- Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: duration}
			}
		})
	}

	done := make(chan struct{})
	go func() {
		wg.Wait()
		close(done)
	}()

	select {
	case <-done:
	case err := <-errs:
		cancelStage()
		<-done
		return err
	case <-ctx.Done():
		cancelStage()
		<-done
		return ctx.Err()
	}
	close(results)
	close(latency)
	// The loaded-latency result carries the transfer stage's own name, so it
	// goes out first: a consumer that keys results by stage keeps the last one
	// it sees, and the transfer result is the stage's headline.
	for res := range latency {
		r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: &res})
	}
	for res := range results {
		r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Direction: res.Direction, Result: &res})
	}
	return nil
}

func (r *runner) warmupGate(ctx context.Context, stage string) <-chan struct{} {
	start := make(chan struct{})
	warmup := adaptiveWarmup(r.cfg.Warmup, r.idleRTT)
	if warmup <= 0 {
		r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "measure"})
		close(start)
		return start
	}
	r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "warmup"})
	go func() {
		timer := time.NewTimer(warmup)
		defer timer.Stop()
		select {
		case <-ctx.Done():
		case <-timer.C:
			r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "measure"})
			close(start)
		}
	}()
	return start
}

func (r *runner) endpoint(path string) (string, error) {
	if path == "" {
		return "", fmt.Errorf("empty endpoint path")
	}
	base := r.cfg.BaseURL
	if r.target != nil {
		base = r.target.Origin
	}
	return httpEndpoint(base, path)
}

// targetTransport is the mechanism the throughput target uses; a runner with no
// discovered target falls back to fetch streams.
func (r *runner) targetTransport() string {
	if r.target == nil {
		return wire.TransportFetchStream
	}
	return r.target.Transport
}

func (r *runner) routes() wire.ThroughputRoutes {
	if r.target != nil {
		return r.target.Routes
	}
	return wire.DefaultThroughputRoutes()
}

// transportOrder is the preference a transport selection resolves to: an
// explicit one on its own, or automatic, which tries preferred then fallback.
// The caller names that pair, and it differs per stage.
func transportOrder(selection string, preferred, fallback string) []string {
	if selection != "auto" {
		return []string{selection}
	}
	return []string{preferred, fallback}
}

// Automatic throughput prefers fetch streams, which still win raw throughput
// over TCP; WebTransport is the explicit choice and the fallback.
func selectTarget(cfg Config, pf wire.Preflight) (*wire.ThroughputTarget, error) {
	// The datagram path is a browser diagnostic; this client drives streams.
	if cfg.ThroughputTransport == wire.TransportWebTransportDatagram {
		return nil, fmt.Errorf("webtransport-datagram throughput is not supported by this client")
	}
	for _, mechanism := range transportOrder(cfg.ThroughputTransport, wire.TransportFetchStream, wire.TransportWebTransport) {
		t, err := selectTargetOver(cfg, pf, mechanism)
		if err == nil {
			return t, nil
		}
		if cfg.ThroughputTransport != "auto" {
			return nil, err
		}
	}
	return nil, fmt.Errorf("%s target unavailable", cfg.ThroughputTarget)
}

func selectTargetOver(cfg Config, pf wire.Preflight, mechanism string) (*wire.ThroughputTarget, error) {
	selection := cfg.ThroughputTarget
	if selection == "auto" {
		for i := range pf.Capabilities.ThroughputTargets {
			t := &pf.Capabilities.ThroughputTargets[i]
			if t.Transport != mechanism {
				continue
			}
			if origin.Equal(t.Origin, cfg.BaseURL) {
				return t, nil
			}
		}
		var candidate *wire.ThroughputTarget
		for i := range pf.Capabilities.ThroughputTargets {
			t := &pf.Capabilities.ThroughputTargets[i]
			if t.Transport == mechanism {
				if candidate != nil {
					return nil, fmt.Errorf("multiple throughput endpoints available; select an origin")
				}
				candidate = t
			}
		}
		if candidate != nil {
			return candidate, nil
		}
	}
	for i := range pf.Capabilities.ThroughputTargets {
		t := &pf.Capabilities.ThroughputTargets[i]
		if t.Transport == mechanism && (t.ID == selection || origin.Equal(t.Origin, selection)) {
			return t, nil
		}
	}
	return nil, fmt.Errorf("%s target unavailable over %s", selection, mechanism)
}

// latencyBusEvidence is the HTTP version the ping chain actually rides. The
// probe that proved the origin reachable is a plain GET over the WebSocket
// client, which is pinned to HTTP/1.1 whatever bus the stage goes on to use —
// so reporting the probe's answer for a WebTransport bus describes an HTTP/1.1
// run that never happened. The datagram bus is HTTP/3 by construction; the
// WebSocket bus really is the HTTP/1.1 the probe observed, since its Upgrade
// handshake cannot ride anything else.
func latencyBusEvidence(target *wire.LatencyTarget, probe *wire.Probe) string {
	if target.Transport == wire.TransportWebTransport {
		return targetProtocolEvidence("http3")
	}
	if probe == nil {
		return ""
	}
	return probe.ProtocolNegotiated
}

func targetProtocolEvidence(protocol string) string {
	switch protocol {
	case "http1":
		return "http/1.1"
	case "http2":
		return "h2"
	case "http3":
		return "h3"
	default:
		return protocol
	}
}

func protocolFromEvidence(protocol string) string {
	switch protocol {
	case "http/1.1", "HTTP/1.1":
		return "http1"
	case "h2", "HTTP/2.0":
		return "http2"
	case "h3", "HTTP/3.0":
		return "http3"
	}
	return protocol
}

func selectLatencyTarget(cfg Config, targets []wire.LatencyTarget) (*wire.LatencyTarget, error) {
	for _, mechanism := range transportOrder(cfg.LatencyTransport, wire.TransportWebTransport, wire.TransportWebSocket) {
		t, err := selectLatencyTargetOver(cfg.LatencyTarget, cfg.BaseURL, targets, mechanism)
		if err == nil {
			return t, nil
		}
		if cfg.LatencyTransport != "auto" {
			return nil, err
		}
	}
	return nil, fmt.Errorf("latency target %q unavailable", cfg.LatencyTarget)
}

func selectLatencyTargetOver(selection, base string, targets []wire.LatencyTarget, mechanism string) (*wire.LatencyTarget, error) {
	var candidate *wire.LatencyTarget
	var sameOriginCandidate *wire.LatencyTarget
	candidateCount := 0
	for i := range targets {
		t := &targets[i]
		if t.Transport != mechanism {
			continue
		}
		if selection != "auto" && (t.ID == selection || origin.Equal(t.Origin, selection)) {
			return t, nil
		}
		if selection == "auto" {
			candidateCount++
			if candidate == nil {
				candidate = t
			}
			if sameOriginCandidate == nil && origin.Equal(t.Origin, base) {
				sameOriginCandidate = t
			}
		}
	}
	if selection == "auto" {
		if sameOriginCandidate != nil {
			return sameOriginCandidate, nil
		}
		if candidateCount == 1 {
			return candidate, nil
		}
		if candidateCount > 1 {
			selection = "ambiguous"
		}
	}
	return nil, fmt.Errorf("latency target %q unavailable", selection)
}

func protocolClient(cfg Config, protocol string, makeHTTP func() *http.Transport) (*http.Client, func()) {
	tlsConfig := &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify} //nolint:gosec
	if protocol == "http3" {
		tr := &http3.Transport{TLSClientConfig: tlsConfig, QUICConfig: transport.NewQUICConfig()}
		return authenticatedClient(cfg, tr), func() { _ = tr.Close() }
	}
	tr := makeHTTP()
	tr.TLSClientConfig = tlsConfig
	if protocol != "negotiated" {
		p := &http.Protocols{}
		p.SetHTTP1(protocol == "http1")
		p.SetHTTP2(protocol == "http2")
		tr.Protocols = p
	}
	return authenticatedClient(cfg, tr), tr.CloseIdleConnections
}
