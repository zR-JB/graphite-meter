package wire

import (
	"bufio"
	"os"
	"strconv"
	"strings"
	"testing"
)

const corpusPath = "../../../api/wire.testvectors.txt"

// vector is one row of api/wire.testvectors.txt: a direction, an input, and the
// expected output. See the file header for the format.
type vector struct {
	line     int
	dir      string // "encode" | "decode"
	input    string
	expected string
}

// loadCorpus parses the shared wire corpus, skipping comment/blank lines. This
// is the same fixture the TS codec asserts against (client wire.test.ts).
func loadCorpus(t *testing.T) []vector {
	t.Helper()
	f, err := os.Open(corpusPath)
	if err != nil {
		t.Fatalf("open corpus: %v", err)
	}
	defer f.Close()

	var vectors []vector
	scanner := bufio.NewScanner(f)
	lineNo := 0
	for scanner.Scan() {
		lineNo++
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.Split(line, "|")
		if len(parts) != 3 {
			t.Fatalf("line %d: want 3 fields, got %d: %q", lineNo, len(parts), line)
		}
		dir := strings.TrimSpace(parts[0])
		if dir != "encode" && dir != "decode" {
			t.Fatalf("line %d: bad dir %q", lineNo, dir)
		}
		vectors = append(vectors, vector{
			line:     lineNo,
			dir:      dir,
			input:    strings.TrimSpace(parts[1]),
			expected: strings.TrimSpace(parts[2]),
		})
	}
	if err := scanner.Err(); err != nil {
		t.Fatalf("scan corpus: %v", err)
	}
	if len(vectors) == 0 {
		t.Fatal("corpus is empty — expected populated test vectors")
	}
	return vectors
}

// TestCodecMatchesCorpus is the byte-exact conformance check both languages run
// against the same file. decode rows assert the parsed frame's canonical render
// (or its rejection code); encode rows assert the on-wire output is byte-exact.
func TestCodecMatchesCorpus(t *testing.T) {
	for _, v := range loadCorpus(t) {
		switch v.dir {
		case "decode":
			f, err := Decode(v.input)
			if want, ok := strings.CutPrefix(v.expected, "ERR:"); ok {
				de, isDecodeErr := err.(*DecodeError)
				if !isDecodeErr {
					t.Errorf("line %d: Decode(%q) = (%+v, %v); want *DecodeError code %q",
						v.line, v.input, f, err, want)
				} else if de.Code != want {
					t.Errorf("line %d: Decode(%q) code = %q; want %q", v.line, v.input, de.Code, want)
				}
				continue
			}
			if err != nil {
				t.Errorf("line %d: Decode(%q) unexpected error: %v", v.line, v.input, err)
				continue
			}
			if got := render(f); got != v.expected {
				t.Errorf("line %d: Decode(%q) rendered\n got  %s\n want %s", v.line, v.input, got, v.expected)
			}
		case "encode":
			f := parseCanonical(t, v.line, v.input)
			if got := Encode(f); got != v.expected {
				t.Errorf("line %d: Encode(%q)\n got  %s\n want %s", v.line, v.input, got, v.expected)
			}
		}
	}
}

// render produces the canonical "op=…;k=v;…" form the decode rows pin. It is the
// test's mirror of the corpus's expected column — kept here, not in the codec,
// since it is a test artifact (the codec only ever emits on-wire frames).
func render(f Frame) string {
	switch f.Op {
	case OpREADY:
		return "op=READY"
	case OpBYE:
		return "op=BYE"
	case OpPING:
		return "op=PING;id=" + u32(f.ID)
	case OpPONG:
		return "op=PONG;id=" + u32(f.ID) + ";nanos=" + u64(f.Nanos)
	case OpSIZE:
		return "op=SIZE;bytes=" + u64(f.Bytes)
	case OpHI:
		return "op=HI;proto=" + f.Proto
	case OpERR:
		return "op=ERR;code=" + f.Code + ";text=" + f.Text
	default:
		return "op=?"
	}
}

// parseCanonical turns an "op=…;k=v;…" spec (the encode rows' input column) into
// a Frame to feed Encode.
func parseCanonical(t *testing.T, line int, spec string) Frame {
	t.Helper()
	var f Frame
	for _, kv := range strings.Split(spec, ";") {
		k, v, _ := strings.Cut(kv, "=")
		switch k {
		case "op":
			f.Op = v
		case "id":
			n, err := strconv.ParseUint(v, 10, 32)
			if err != nil {
				t.Fatalf("line %d: bad id %q: %v", line, v, err)
			}
			f.ID = uint32(n)
		case "nanos":
			f.Nanos = mustU64(t, line, v)
		case "bytes":
			f.Bytes = mustU64(t, line, v)
		case "proto":
			f.Proto = v
		case "code":
			f.Code = v
		case "text":
			f.Text = v
		default:
			t.Fatalf("line %d: unknown canonical key %q", line, k)
		}
	}
	return f
}

func mustU64(t *testing.T, line int, v string) uint64 {
	t.Helper()
	n, err := strconv.ParseUint(v, 10, 64)
	if err != nil {
		t.Fatalf("line %d: bad uint %q: %v", line, v, err)
	}
	return n
}
