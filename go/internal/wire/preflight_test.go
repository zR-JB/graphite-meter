package wire

import (
	"bytes"
	"encoding/json/v2"
	"os"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

// loadSchema compiles the cross-language schema used by Go structs and golden documents.
func loadSchema(t *testing.T, name string) *jsonschema.Schema {
	t.Helper()
	raw, err := os.ReadFile("../../../api/" + name + ".schema.json")
	if err != nil {
		t.Fatalf("read %s schema: %v", name, err)
	}
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(raw))
	if err != nil {
		t.Fatalf("parse %s schema: %v", name, err)
	}
	c := jsonschema.NewCompiler()
	if err := c.AddResource(name+".schema.json", doc); err != nil {
		t.Fatalf("add %s schema: %v", name, err)
	}
	s, err := c.Compile(name + ".schema.json")
	if err != nil {
		t.Fatalf("compile %s schema: %v", name, err)
	}
	return s
}

func mustValidate(t *testing.T, s *jsonschema.Schema, data []byte) {
	t.Helper()
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(data))
	if err != nil {
		t.Fatalf("parse document: %v\n%s", err, data)
	}
	if err := s.Validate(doc); err != nil {
		t.Fatalf("schema validation: %v\n%s", err, data)
	}
}

func TestGoldenDocumentsMatchTheirSchemas(t *testing.T) {
	for _, name := range []string{"preflight", "probe"} {
		t.Run(name, func(t *testing.T) {
			data, err := os.ReadFile("../../../api/" + name + ".golden.json")
			if err != nil {
				t.Fatalf("read %s golden: %v", name, err)
			}
			mustValidate(t, loadSchema(t, name), data)
		})
	}
}

func TestMarshaledStructsMatchTheirSchemas(t *testing.T) {
	throughput := ThroughputTarget{ID: "http1-clear", Origin: "http://speed.example:7246", Transport: "fetch-stream", Protocol: "http1", Routes: DefaultThroughputRoutes()}
	latency := LatencyTarget{ID: "ws-http1-clear", Origin: throughput.Origin, Transport: "websocket", Protocol: "http1", Routes: DefaultLatencyRoutes()}
	values := []struct {
		name  string
		value any
	}{
		{"preflight", Preflight{Server: ServerInfo{Name: "graphite-meter"}, EngineVersion: "test", Generation: "test-generation", Capabilities: Capabilities{ThroughputTargets: []ThroughputTarget{throughput}, LatencyTargets: []LatencyTarget{latency}}}},
		{"probe", Probe{ClientIP: "198.51.100.4", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "h2", Load: &ProbeLoad{Active: 1, Max: 256}}},
	}
	for _, tc := range values {
		t.Run(tc.name, func(t *testing.T) {
			data, err := json.Marshal(tc.value)
			if err != nil {
				t.Fatalf("marshal %s: %v", tc.name, err)
			}
			mustValidate(t, loadSchema(t, tc.name), data)
		})
	}
}

func TestTargetsRequireExplicitTransport(t *testing.T) {
	for _, transport := range []string{"", "null", `""`, `"udp"`} {
		field := ""
		if transport != "" {
			field = `,"transport":` + transport
		}
		var throughput ThroughputTarget
		if err := json.Unmarshal([]byte(`{"baseUrl":".","protocol":"http2"`+field+`}`), &throughput); err == nil {
			t.Fatalf("accepted throughput transport %q", transport)
		}
		var latency LatencyTarget
		if err := json.Unmarshal([]byte(`{"baseUrl":"."`+field+`}`), &latency); err == nil {
			t.Fatalf("accepted latency transport %q", transport)
		}
	}
}

func TestTargetJSONUsesStrictNativeV2Decoding(t *testing.T) {
	tests := []struct {
		name, document string
	}{
		{"duplicate name", `{"baseUrl":"https://one.example","baseUrl":"https://two.example"}`},
		{"invalid UTF-8", "{\"baseUrl\":\"\xff\"}"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var target ThroughputTarget
			if err := json.Unmarshal([]byte(test.document), &target); err == nil {
				t.Fatalf("accepted malformed JSON %s", test.document)
			}
		})
	}

	var target ThroughputTarget
	if err := json.Unmarshal([]byte(`{"BaseUrl":"https://speed.example:7246"}`), &target); err == nil {
		t.Fatal("accepted missing case-sensitive baseUrl")
	}
	if target.Origin != "" {
		t.Fatalf("case-insensitive field match populated Origin=%q", target.Origin)
	}
}

// A latency target's protocol never crosses the wire; it follows the transport.
func TestLatencyTargetProtocolFollowsItsTransport(t *testing.T) {
	for transport, want := range map[string]string{
		TransportWebSocket:    "http1",
		TransportWebTransport: "http3",
	} {
		var target LatencyTarget
		document := `{"baseUrl":"https://speed.example:7246","transport":"` + transport + `"}`
		if err := json.Unmarshal([]byte(document), &target); err != nil {
			t.Fatalf("unmarshal %q: %v", transport, err)
		}
		if got := target.Protocol; got != want {
			t.Errorf("transport %q derived protocol %q, want %q", transport, got, want)
		}
	}
}

func TestPreflightGoldenSurvivesARoundTrip(t *testing.T) {
	var pf Preflight
	data, err := os.ReadFile("../../../api/preflight.golden.json")
	if err != nil {
		t.Fatalf("read preflight golden: %v", err)
	}
	if err := json.Unmarshal(data, &pf); err != nil {
		t.Fatalf("unmarshal preflight golden: %v", err)
	}
	if got, want := pf.Capabilities.LatencyTargets[0].Routes.Ping, "/ws/ping"; got != want {
		t.Fatalf("LatencyTargets[0].Routes.Ping = %q, want %q", got, want)
	}
	data, err = json.Marshal(pf)
	if err != nil {
		t.Fatalf("marshal preflight: %v", err)
	}
	mustValidate(t, loadSchema(t, "preflight"), data)
}

func TestTargetOriginsAndCapabilitiesAreValidated(t *testing.T) {
	for _, origin := range []string{"https://u:p@example.com", "https://example.com/", "https://example.com/path", "https://example.com?", "https://example.com#", "//example.com", "ftp://example.com", "https://example.com:99999"} {
		data, err := json.Marshal(map[string]string{"baseUrl": origin, "protocol": "http1", "transport": TransportFetchStream})
		if err != nil {
			t.Fatal(err)
		}
		var target ThroughputTarget
		if err := json.Unmarshal(data, &target); err == nil {
			t.Errorf("accepted origin %q", origin)
		}
	}
	for _, document := range []string{`{"baseUrl":".","protocol":"http4","transport":"fetch-stream"}`, `{"baseUrl":".","protocol":"http1","transport":"udp"}`} {
		var target ThroughputTarget
		if err := json.Unmarshal([]byte(document), &target); err == nil {
			t.Errorf("accepted target %s", document)
		}
	}
	for _, origin := range []string{".", "https://[::1]:7247", "http://other.example:7246"} {
		data, err := json.Marshal(map[string]string{"baseUrl": origin, "protocol": "http1", "transport": TransportFetchStream})
		if err != nil {
			t.Fatal(err)
		}
		var target ThroughputTarget
		if err := json.Unmarshal(data, &target); err != nil {
			t.Errorf("rejected origin %q: %v", origin, err)
		}
	}
}

func TestDiscoveryMetadataAndProbeEvidenceBounds(t *testing.T) {
	valid := Preflight{Generation: "a", Capabilities: Capabilities{ThroughputTargets: []ThroughputTarget{}, LatencyTargets: []LatencyTarget{}}}
	if err := valid.Validate(); err != nil {
		t.Fatal(err)
	}
	for _, invalid := range []Preflight{{}, {Generation: "a"}, {Generation: "a", Capabilities: Capabilities{ThroughputTargets: make([]ThroughputTarget, 33), LatencyTargets: []LatencyTarget{}}}} {
		if err := invalid.Validate(); err == nil {
			t.Fatal("accepted invalid discovery")
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
