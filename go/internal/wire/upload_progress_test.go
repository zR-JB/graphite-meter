package wire

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
)

func TestUploadProgressConformance(t *testing.T) {
	var cases []struct {
		Name   string         `json:"name"`
		Record jsontext.Value `json:"record"`
		Valid  bool           `json:"valid"`
	}
	if err := json.Unmarshal(apipin.Read(t, "upload-progress.testvectors.json"), &cases); err != nil {
		t.Fatal(err)
	}
	for _, tc := range cases {
		t.Run(tc.Name, func(t *testing.T) {
			event, err := DecodeUploadProgress(tc.Record)
			if (err == nil) != tc.Valid {
				t.Fatalf("decode %s = %+v, %v; valid=%t", tc.Record, event, err, tc.Valid)
			}
			// A checkpoint's counters obey the same contract as a counter record's.
			var kind struct {
				Type string `json:"type"`
			}
			if json.Unmarshal(tc.Record, &kind) == nil && (kind.Type == "progress" || kind.Type == "complete") {
				var c UploadCheckpoint
				err := json.Unmarshal(tc.Record, &c)
				if (err == nil) != tc.Valid || err == nil && (c.Bytes != event.Bytes || c.Nanos != event.Nanos) {
					t.Fatalf("checkpoint %s = %+v, %v; valid=%t", tc.Record, c, err, tc.Valid)
				}
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
