package wire

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"os"
	"testing"
)

func TestUploadProgressConformance(t *testing.T) {
	data, err := os.ReadFile("../../../api/upload-progress.testvectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Name   string         `json:"name"`
		Record jsontext.Value `json:"record"`
		Valid  bool           `json:"valid"`
	}
	if err := json.Unmarshal(data, &cases); err != nil {
		t.Fatal(err)
	}
	for _, tc := range cases {
		t.Run(tc.Name, func(t *testing.T) {
			event, err := DecodeUploadProgress(tc.Record)
			if (err == nil) != tc.Valid {
				t.Fatalf("decode %s = %+v, %v; valid=%t", tc.Record, event, err, tc.Valid)
			}
			if tc.Valid {
				encoded, err := json.Marshal(event)
				if err != nil {
					t.Fatal(err)
				}
				decoded, err := DecodeUploadProgress(encoded)
				if err != nil || decoded != event {
					t.Fatalf("round trip = %+v, %v; want %+v", decoded, err, event)
				}
			}
		})
	}
}

func TestUploadProgressExplicitZeroCounters(t *testing.T) {
	for _, kind := range []string{"progress", "complete"} {
		encoded, err := json.Marshal(UploadProgress{Type: kind})
		if err != nil {
			t.Fatal(err)
		}
		var raw map[string]jsontext.Value
		if err := json.Unmarshal(encoded, &raw); err != nil {
			t.Fatal(err)
		}
		if string(raw["bytes"]) != "0" || string(raw["nanos"]) != "0" {
			t.Fatalf("zero receiver window omitted counters: %s", encoded)
		}
	}
}

func TestUploadProgressCannotEmitInexactCounters(t *testing.T) {
	for _, event := range []UploadProgress{
		{Type: "progress", Bytes: maxUploadCounter + 1},
		{Type: "complete", Nanos: maxUploadCounter + 1},
	} {
		if _, err := json.Marshal(event); err == nil {
			t.Fatalf("emitted inexact receiver counters: %+v", event)
		}
	}
}
