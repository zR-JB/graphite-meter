package wire

import (
	"bytes"
	"encoding/json/v2"
	"os"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

// loadSchema compiles api/<name>.schema.json, the cross-language contract the
// Go structs and the golden documents are both pinned to.
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

// api/wire.md calls `transport` a required field but keeps a back-compat path
// for documents written before it existed: a target with no transport reads as
// the default its list always carried. Dropping the default leaves the field
// empty, and a client selecting by (baseUrl, transport) matches no target at all.
func TestTargetsWithoutATransportKeepTheirLegacyDefault(t *testing.T) {
	var throughput ThroughputTarget
	if err := json.Unmarshal([]byte(`{"baseUrl":"https://speed.example:7246","protocol":"http2"}`), &throughput); err != nil {
		t.Fatalf("unmarshal throughput: %v", err)
	}
	if got := throughput.Transport; got != TransportFetchStream {
		t.Errorf("throughput transport = %q, want the pre-transport default %q", got, TransportFetchStream)
	}

	var latency LatencyTarget
	if err := json.Unmarshal([]byte(`{"baseUrl":"https://speed.example:7246"}`), &latency); err != nil {
		t.Fatalf("unmarshal latency: %v", err)
	}
	if got := latency.Transport; got != TransportWebSocket {
		t.Errorf("latency transport = %q, want the pre-transport default %q", got, TransportWebSocket)
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
	if err := json.Unmarshal([]byte(`{"BaseUrl":"https://speed.example:7246"}`), &target); err != nil {
		t.Fatal(err)
	}
	if target.Origin != "" {
		t.Fatalf("case-insensitive field match populated Origin=%q", target.Origin)
	}
}

// A latency target's protocol never crosses the wire; it follows the transport.
// WebTransport is an HTTP/3 extended CONNECT, so reporting http1 for it would
// mislabel every result the WebTransport ping bus produces.
func TestLatencyTargetProtocolFollowsItsTransport(t *testing.T) {
	for transport, want := range map[string]string{
		TransportWebSocket:    "http1",
		TransportWebTransport: "http3",
		"":                    "http1",
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

// TestPreflightGoldenSurvivesARoundTrip pins that decoding restores the
// client-side route fields json:"-" keeps off the wire, and that re-encoding
// still satisfies the schema.
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
