package wire

import (
	"os"
	"strconv"
	"strings"
	"testing"
)

func TestCodecMatchesCorpus(t *testing.T) {
	corpus, err := os.ReadFile("../../../api/wire.testvectors.txt")
	if err != nil {
		t.Fatal(err)
	}
	for line := range strings.SplitSeq(string(corpus), "\n") {
		if strings.HasPrefix(line, "#") || line == "" {
			continue
		}
		parts := strings.Split(line, "|")
		if len(parts) != 3 {
			t.Fatalf("malformed corpus row: %s", line)
		}
		op, input, expected := strings.TrimSpace(parts[0]), strings.TrimSpace(parts[1]), strings.TrimSpace(parts[2])
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
