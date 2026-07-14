package wire

import (
	"bytes"
	"encoding/json"
	"os"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

func schema(t *testing.T, name string) *jsonschema.Schema {
	t.Helper()
	raw, err := os.ReadFile("../../../api/" + name + ".schema.json")
	if err != nil {
		t.Fatal(err)
	}
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(raw))
	if err != nil {
		t.Fatal(err)
	}
	c := jsonschema.NewCompiler()
	if err := c.AddResource(name+".schema.json", doc); err != nil {
		t.Fatal(err)
	}
	s, err := c.Compile(name + ".schema.json")
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func valid(t *testing.T, s *jsonschema.Schema, data []byte) {
	t.Helper()
	v, err := jsonschema.UnmarshalJSON(bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	if err := s.Validate(v); err != nil {
		t.Fatalf("schema validation: %v\n%s", err, data)
	}
}

func TestGoldenConformance(t *testing.T) {
	for _, name := range []string{"preflight", "probe"} {
		t.Run(name, func(t *testing.T) {
			data, err := os.ReadFile("../../../api/" + name + ".golden.json")
			if err != nil {
				t.Fatal(err)
			}
			valid(t, schema(t, name), data)
		})
	}
}

func TestStructConformance(t *testing.T) {
	h1 := ThroughputTarget{ID: "http1-clear", Origin: "http://speed.example:7246", Transport: "fetch-stream", Protocol: "http1", Routes: DefaultThroughputRoutes()}
	ws := LatencyTarget{ID: "ws-http1-clear", Origin: h1.Origin, Transport: "websocket", Protocol: "http1", Routes: DefaultLatencyRoutes()}
	values := []struct {
		name  string
		value any
	}{
		{"preflight", Preflight{Server: ServerInfo{Name: "graphite-meter", Host: "speed.example", Port: 7246}, EngineVersion: "test", Capabilities: Capabilities{ThroughputTargets: []ThroughputTarget{h1}, LatencyTargets: []LatencyTarget{ws}}}},
		{"probe", Probe{ClientIP: "198.51.100.4", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "h2"}},
	}
	for _, tc := range values {
		t.Run(tc.name, func(t *testing.T) {
			data, err := json.Marshal(tc.value)
			if err != nil {
				t.Fatal(err)
			}
			valid(t, schema(t, tc.name), data)
		})
	}
}

func TestGoldenRoundTrips(t *testing.T) {
	var pf Preflight
	data, err := os.ReadFile("../../../api/preflight.golden.json")
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(data, &pf); err != nil {
		t.Fatal(err)
	}
	if got := pf.Capabilities.LatencyTargets[0].Routes.Ping; got != "/ws/ping" {
		t.Fatal("websocket route lost")
	}
	data, _ = json.Marshal(pf)
	valid(t, schema(t, "preflight"), data)
}
