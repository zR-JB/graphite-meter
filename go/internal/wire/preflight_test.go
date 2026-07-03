package wire

import (
	"bytes"
	"encoding/json"
	"os"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

const (
	schemaPath = "../../../api/preflight.schema.json"
	goldenPath = "../../../api/preflight.golden.json"
)

// compileSchema loads api/preflight.schema.json — the single source of truth
// for Surface A — and compiles it.
func compileSchema(t *testing.T) *jsonschema.Schema {
	t.Helper()
	raw, err := os.ReadFile(schemaPath)
	if err != nil {
		t.Fatalf("read schema: %v", err)
	}
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(raw))
	if err != nil {
		t.Fatalf("unmarshal schema: %v", err)
	}
	c := jsonschema.NewCompiler()
	if err := c.AddResource("preflight.schema.json", doc); err != nil {
		t.Fatalf("add resource: %v", err)
	}
	sch, err := c.Compile("preflight.schema.json")
	if err != nil {
		t.Fatalf("compile schema: %v", err)
	}
	return sch
}

// validate checks JSON bytes against the schema.
func validate(t *testing.T, sch *jsonschema.Schema, data []byte) {
	t.Helper()
	inst, err := jsonschema.UnmarshalJSON(bytes.NewReader(data))
	if err != nil {
		t.Fatalf("unmarshal instance: %v", err)
	}
	if err := sch.Validate(inst); err != nil {
		t.Fatalf("schema validation failed:\n%v\ninstance: %s", err, data)
	}
}

// TestGoldenMatchesSchema guards the checked-in example body.
func TestGoldenMatchesSchema(t *testing.T) {
	sch := compileSchema(t)
	golden, err := os.ReadFile(goldenPath)
	if err != nil {
		t.Fatalf("read golden: %v", err)
	}
	validate(t, sch, golden)
}

// TestStructMarshalsToValidSchema guards the hand-written Go struct: anything it
// can produce must satisfy the schema. This is the drift detector — rename a
// field or a json tag and this fails.
func TestStructMarshalsToValidSchema(t *testing.T) {
	sch := compileSchema(t)

	h1 := "http://speed.example:8080"
	p := Preflight{
		ClientIP: "198.51.100.4",
		Server: ServerInfo{
			Name:     "graphite-meter",
			Host:     "speed.example",
			Port:     8080,
			Location: "ams",
		},
		PreTestPingMs:      0,
		EngineVersion:      "0.1.0-test",
		ProtocolNegotiated: "http/1.1",
		Capabilities: Capabilities{
			Origins:    Origins{H1: &h1, TLS: nil, H3: nil},
			Transports: Transports{FetchStream: false, WebSocket: false, WebTransport: false},
			Endpoints:  DefaultEndpoints(),
		},
	}
	data, err := json.Marshal(p)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	validate(t, sch, data)

	// Server.Location is omitempty: omitting it must still validate.
	p.Server.Location = ""
	data, err = json.Marshal(p)
	if err != nil {
		t.Fatalf("marshal (no location): %v", err)
	}
	validate(t, sch, data)
}

// TestGoldenRoundTripsThroughStruct ensures the golden decodes into the struct
// and re-encodes to a schema-valid document (no field lost in translation).
func TestGoldenRoundTripsThroughStruct(t *testing.T) {
	sch := compileSchema(t)
	golden, err := os.ReadFile(goldenPath)
	if err != nil {
		t.Fatalf("read golden: %v", err)
	}
	var p Preflight
	if err := json.Unmarshal(golden, &p); err != nil {
		t.Fatalf("unmarshal golden into struct: %v", err)
	}
	data, err := json.Marshal(p)
	if err != nil {
		t.Fatalf("re-marshal: %v", err)
	}
	validate(t, sch, data)

	// Spot-check a couple of fields survived the round-trip.
	if p.Capabilities.Endpoints.WTPing != "/wt/ping" {
		t.Errorf("wtPing = %q, want /wt/ping", p.Capabilities.Endpoints.WTPing)
	}
	if p.Capabilities.Origins.TLS != nil {
		t.Errorf("tls origin = %v, want null", *p.Capabilities.Origins.TLS)
	}
}
