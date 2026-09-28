package wire

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"reflect"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
)

func TestGoldenDocumentsMatchTheirSchemas(t *testing.T) {
	for name, value := range map[string]any{"preflight": new(Preflight), "probe": new(Probe)} {
		t.Run(name, func(t *testing.T) {
			data := apipin.Read(t, name+".golden.json")
			schema := apipin.Schema(t, name)
			apipin.Validate(t, schema, data)
			if err := json.Unmarshal(data, value); err != nil {
				t.Fatalf("unmarshal %s golden: %v", name, err)
			}
			data, err := json.Marshal(value)
			if err != nil {
				t.Fatalf("marshal %s: %v", name, err)
			}
			apipin.Validate(t, schema, data)
		})
	}
}

// Targets name an explicit transport; a latency target's protocol never crosses the wire and follows it.
func TestTargetOriginsAndCapabilitiesAreValidated(t *testing.T) {
	fetch := func(origin string) string {
		data, err := json.Marshal(map[string]string{"baseUrl": origin, "protocol": "http1",
			"transport": TransportFetchStream})
		if err != nil {
			t.Fatal(err)
		}
		return string(data)
	}
	for _, tc := range []struct {
		latency  bool
		document string
		want     string // the decoded protocol, "skipped" for a newer server's target, or "" when refused
	}{
		{false, fetch("https://u:p@example.com"), ""},
		{false, fetch("https://example.com/"), ""},
		{false, fetch("https://example.com/path"), ""},
		{false, fetch("https://example.com?"), ""},
		{false, fetch("https://example.com#"), ""},
		{false, fetch("//example.com"), ""},
		{false, fetch("ftp://example.com"), ""},
		{false, fetch("https://example.com:99999"), ""},
		{false, fetch("."), "http1"},
		{false, fetch("https://[::1]:7247"), "http1"},
		{false, fetch("http://other.example:7246"), "http1"},
		{false, `{"baseUrl":".","protocol":"http4","transport":"fetch-stream"}`, "skipped"},
		{false, `{"baseUrl":".","protocol":"http2"}`, ""},
		{false, `{"baseUrl":".","protocol":"http2","transport":null}`, ""},
		{false, `{"baseUrl":".","protocol":"http2","transport":""}`, ""},
		{false, `{"baseUrl":".","protocol":"http2","transport":"udp"}`, "skipped"},
		{false, `{"baseUrl":"https://one.example","baseUrl":"https://two.example","protocol":"http1",` +
			`"transport":"fetch-stream"}`, ""},
		{true, `{"baseUrl":"."}`, ""},
		{true, `{"baseUrl":".","transport":null}`, ""},
		{true, `{"baseUrl":".","transport":""}`, ""},
		{true, `{"baseUrl":".","transport":"udp"}`, "skipped"},
		{true, `{"baseUrl":"https://speed.example:7246","transport":"websocket"}`, "http1"},
		{true, `{"baseUrl":"https://speed.example:7246","transport":"webtransport"}`, "http3"},
	} {
		var protocol, transport string
		var err error
		if tc.latency {
			var target LatencyTarget
			err = json.Unmarshal([]byte(tc.document), &target)
			protocol, transport = target.Protocol, target.Transport
		} else {
			var target ThroughputTarget
			err = json.Unmarshal([]byte(tc.document), &target)
			protocol, transport = target.Protocol, target.Transport
		}
		got := protocol
		if err != nil {
			got = ""
		} else if transport == "" {
			got = "skipped"
		}
		if got != tc.want {
			t.Errorf("%s = %q (%v), want %q", tc.document, got, err, tc.want)
		}
	}
}

func TestNewerServersTargetsAreSkipped(t *testing.T) {
	raw := apipin.Read(t, "preflight.forward.golden.json")
	var golden struct {
		Document jsontext.Value `json:"document"`
		Decoded  jsontext.Value `json:"decoded"`
	}
	if err := json.Unmarshal(raw, &golden); err != nil {
		t.Fatal(err)
	}
	var p Preflight
	if err := json.Unmarshal(golden.Document, &p); err != nil {
		t.Fatalf("decode a newer server's preflight: %v", err)
	}
	if err := p.Validate(); err != nil {
		t.Fatal(err)
	}
	decoded, err := json.Marshal(struct {
		Throughput []ThroughputTarget `json:"throughput"`
		Latency    []LatencyTarget    `json:"latency"`
	}{p.Capabilities.ThroughputTargets, p.Capabilities.LatencyTargets})
	if err != nil {
		t.Fatal(err)
	}
	var got, want any
	if json.Unmarshal(decoded, &got) != nil || json.Unmarshal(golden.Decoded, &want) != nil ||
		!reflect.DeepEqual(got, want) {
		t.Fatalf("decoded targets %s, want %s", decoded, golden.Decoded)
	}
}

func TestDiscoveryMetadataAndProbeEvidenceBounds(t *testing.T) {
	valid := Preflight{Generation: "a",
		Capabilities: Capabilities{ThroughputTargets: []ThroughputTarget{}, LatencyTargets: []LatencyTarget{}}}
	if err := valid.Validate(); err != nil {
		t.Fatal(err)
	}
	for _, invalid := range []Preflight{{}, {Generation: "a"},
		{Generation: "a",
			Capabilities: Capabilities{ThroughputTargets: make([]ThroughputTarget, 33),
				LatencyTargets: []LatencyTarget{}}}} {
		if err := invalid.Validate(); err == nil {
			t.Fatal("accepted invalid discovery")
		}
	}
	// A server that predates the stage limit allows the old five minutes; an advertised one stays within a day.
	if limit := valid.Capabilities.StageLimit(); limit != DefaultStageLimit {
		t.Fatalf("absent stage limit = %v, want %v", limit, DefaultStageLimit)
	}
	for limit, ok := range map[int64]bool{999: false, 1000: true, 7_200_000: true, 86_400_000: true,
		86_400_001: false, -1: false} {
		limited := valid
		limited.Capabilities.MaxStageMs = limit
		if err := limited.Validate(); (err == nil) != ok {
			t.Fatalf("stage limit %d ms: Validate() = %v", limit, err)
		}
		if ok && limited.Capabilities.StageLimit() != time.Duration(limit)*time.Millisecond {
			t.Fatalf("stage limit %d ms reads %v", limit, limited.Capabilities.StageLimit())
		}
	}
	probe := Probe{ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "h2"}
	if err := probe.Validate(); err != nil {
		t.Fatal(err)
	}
	for _, load := range []*ProbeLoad{{Active: -1, Max: 2}, {Active: 0, Max: 0}} {
		probe.Load = load
		if err := probe.Validate(); err == nil {
			t.Fatal("accepted invalid occupancy")
		}
	}
}
