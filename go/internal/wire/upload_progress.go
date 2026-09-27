package wire

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"math"
)

// UploadProgress is one receiver record: receiver bytes and nanoseconds since its first chunk.
type UploadProgress struct {
	Type    string `json:"type"`
	Bytes   uint64 `json:"bytes"`
	Nanos   uint64 `json:"nanos"`
	Message string `json:"message,omitempty"`
	Code    string `json:"code,omitempty"`
}

// maxUploadCounter keeps JSON counters exact in both supported clients.
const maxUploadCounter = 1<<53 - 1

type UploadSession struct {
	UploadID string `json:"uploadId"`
}

func (s UploadSession) Validate() error {
	if s.UploadID == "" || len(s.UploadID) > 8192 {
		return errors.New("invalid uploadId")
	}
	return nil
}

type UploadCheckpoint struct {
	Bytes uint64 `json:"bytes"`
	Nanos uint64 `json:"nanos"`
}

func (c *UploadCheckpoint) UnmarshalJSON(data []byte) error {
	var raw map[string]jsontext.Value
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	return decodeCounters(raw, &c.Bytes, &c.Nanos)
}

// decodeCounters holds both counters to the progress contract: present, integral and exact in JSON.
func decodeCounters(raw map[string]jsontext.Value, bytes, nanos *uint64) error {
	for name, dst := range map[string]*uint64{"bytes": bytes, "nanos": nanos} {
		value := raw[name]
		var number float64
		if value.Kind() != '0' || json.Unmarshal(value, &number) != nil || number < 0 ||
			number > maxUploadCounter || math.Trunc(number) != number {
			return errors.New("invalid upload progress counter")
		}
		*dst = uint64(number) // bounded above by the largest exact JSON integer
	}
	return nil
}

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

// DecodeUploadProgress rejects malformed records; a missing counter differs from zero.
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
		event.Message, event.Code = CleanText(event.Message, 256), CleanText(event.Code, 64)
		return event, nil
	case "progress", "complete":
		if err := decodeCounters(raw, &event.Bytes, &event.Nanos); err != nil {
			return UploadProgress{}, err
		}
		return event, nil
	}
	return UploadProgress{}, errors.New("invalid upload progress record")
}
