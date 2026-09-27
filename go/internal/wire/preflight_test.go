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
	for name, value := range map[string]any{"preflight": new(Preflight), "probe": new(Probe)} {
		t.Run(name, func(t *testing.T) {
			data, err := os.ReadFile("../../../api/" + name + ".golden.json")
			if err != nil {
				t.Fatalf("read %s golden: %v", name, err)
			}
			schema := loadSchema(t, name)
			mustValidate(t, schema, data)
			if err := json.Unmarshal(data, value); err != nil {
				t.Fatalf("unmarshal %s golden: %v", name, err)
			}
			if data, err = json.Marshal(value); err != nil {
				t.Fatalf("marshal %s: %v", name, err)
			}
			mustValidate(t, schema, data)
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
		protocol string // the decoded protocol, or empty when the target must be refused
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
		{false, `{"baseUrl":".","protocol":"http4","transport":"fetch-stream"}`, ""},
		{false, `{"baseUrl":".","protocol":"http2"}`, ""},
		{false, `{"baseUrl":".","protocol":"http2","transport":null}`, ""},
		{false, `{"baseUrl":".","protocol":"http2","transport":""}`, ""},
		{false, `{"baseUrl":".","protocol":"http2","transport":"udp"}`, ""},
		{false, `{"baseUrl":"https://one.example","baseUrl":"https://two.example","protocol":"http1",` +
			`"transport":"fetch-stream"}`, ""},
		{true, `{"baseUrl":"."}`, ""},
		{true, `{"baseUrl":".","transport":null}`, ""},
		{true, `{"baseUrl":".","transport":""}`, ""},
		{true, `{"baseUrl":".","transport":"udp"}`, ""},
		{true, `{"baseUrl":"https://speed.example:7246","transport":"websocket"}`, "http1"},
		{true, `{"baseUrl":"https://speed.example:7246","transport":"webtransport"}`, "http3"},
	} {
		var protocol string
		var err error
		if tc.latency {
			var target LatencyTarget
			err = json.Unmarshal([]byte(tc.document), &target)
			protocol = target.Protocol
		} else {
			var target ThroughputTarget
			err = json.Unmarshal([]byte(tc.document), &target)
			protocol = target.Protocol
		}
		if (err == nil) != (tc.protocol != "") || protocol != tc.protocol {
			t.Errorf("%s = protocol %q, %v; want %q", tc.document, protocol, err, tc.protocol)
		}
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
