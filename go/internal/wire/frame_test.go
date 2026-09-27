package wire

import (
	"strconv"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
)

func TestCodecMatchesCorpus(t *testing.T) {
	for _, row := range apipin.Rows(t, "wire.testvectors.txt", 3) {
		op, input, expected := row[0], row[1], row[2]
		t.Run(op+"/"+input, func(t *testing.T) {
			var actual string
			var err error
			switch op {
			case "encode-ping":
				id, parseErr := strconv.ParseUint(input, 10, 32)
				if parseErr != nil {
					t.Fatal(parseErr)
				}
				actual = EncodePing(uint32(id))
			case "encode-pong":
				values := strings.Split(input, ",")
				id, idErr := strconv.ParseUint(values[0], 10, 32)
				handling, handlingErr := strconv.ParseUint(values[1], 10, 64)
				if idErr != nil || handlingErr != nil {
					t.Fatalf("invalid fixture %s", input)
				}
				actual = EncodePong(uint32(id), handling)
			case "decode-ping":
				var id uint32
				id, err = DecodePing(input)
				actual = strconv.FormatUint(uint64(id), 10)
			case "decode-pong":
				var pong Pong
				pong, err = DecodePong(input)
				actual = strconv.FormatUint(uint64(pong.ID), 10) + "," + strconv.FormatUint(pong.HandlingNanos, 10)
			default:
				t.Fatalf("unknown corpus operation %s", op)
			}
			if expected == "INVALID" {
				if err == nil {
					t.Fatalf("accepted malformed message %q", input)
				}
			} else if err != nil || actual != expected {
				t.Fatalf("got %q (%v), want %q", actual, err, expected)
			}
		})
	}
}
