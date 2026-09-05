package wire

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"math"
)

// UploadProgress describes one owner-bound upload aggregate. Bytes are receiver
// bytes and Nanos is elapsed receiver time from its first accepted chunk.
// Both counters are explicit, including zero; ready/error do not measure data.
type UploadProgress struct {
	Type    string `json:"type"`
	Bytes   uint64 `json:"bytes"`
	Nanos   uint64 `json:"nanos"`
	Message string `json:"message,omitempty"`
	Code    string `json:"code,omitempty"`
}

// maxUploadCounter keeps JSON counters exact in both supported clients.
const maxUploadCounter = 1<<53 - 1

// MarshalJSONTo keeps control records separate from counter observations.
func (p UploadProgress) MarshalJSONTo(out *jsontext.Encoder) error {
	if p.Type == "progress" || p.Type == "complete" {
		if p.Bytes > maxUploadCounter || p.Nanos > maxUploadCounter {
			return errors.New("upload progress counter exceeds exact JSON range")
		}
		type counters UploadProgress
		return json.MarshalEncode(out, counters(p))
	}
	return json.MarshalEncode(out, struct {
		Type    string `json:"type"`
		Message string `json:"message,omitempty"`
		Code    string `json:"code,omitempty"`
	}{Type: p.Type, Message: p.Message, Code: p.Code})
}

// DecodeUploadProgress rejects malformed records without inventing observations.
// Missing counters differ from an explicit zero-length receiver window.
func DecodeUploadProgress(data []byte) (UploadProgress, error) {
	var raw map[string]jsontext.Value
	if err := json.Unmarshal(data, &raw); err != nil {
		return UploadProgress{}, err
	}
	var event UploadProgress
	if err := json.Unmarshal(raw["type"], &event.Type); err != nil {
		return UploadProgress{}, err
	}
	switch event.Type {
	case "ready":
		return event, nil
	case "error":
		for name, dst := range map[string]*string{"message": &event.Message, "code": &event.Code} {
			if value, ok := raw[name]; ok {
				if value.Kind() != '"' {
					return UploadProgress{}, errors.New("invalid upload refusal detail")
				}
				if err := json.Unmarshal(value, dst); err != nil {
					return UploadProgress{}, err
				}
			}
		}
		return event, nil
	case "progress", "complete":
		for name, dst := range map[string]*uint64{"bytes": &event.Bytes, "nanos": &event.Nanos} {
			value := raw[name]
			var number float64
			if value.Kind() != '0' || json.Unmarshal(value, &number) != nil || number < 0 || number > maxUploadCounter || math.Trunc(number) != number {
				return UploadProgress{}, errors.New("invalid upload progress counter")
			}
			*dst = uint64(number) // bounded above by the largest exact JSON integer
		}
		return event, nil
	}
	return UploadProgress{}, errors.New("invalid upload progress record")
}
