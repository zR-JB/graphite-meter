package wire

import (
	"errors"
	"io"
	"strings"
	"testing"
)

func TestEncodeStreamPreamble(t *testing.T) {
	if got, want := EncodeStreamPreamble(Frame{Op: OpSIZE, Bytes: 1048576}), "SIZE,1048576\n"; got != want {
		t.Errorf("EncodeStreamPreamble = %q; want %q", got, want)
	}
}

func TestReadStreamPreamble(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  Frame
		rest  string
		code  string
		err   error
	}{
		{name: "size", input: "SIZE,1048576\npayload", want: Frame{Op: OpSIZE, Bytes: 1048576}, rest: "payload"},
		{name: "empty args", input: "SIZE,\n", code: ErrBadArgs},
		{name: "unknown op", input: "NOPE\n", code: ErrBadOp},
		{name: "no newline", input: "SIZE,1", err: io.EOF},
		{name: "overlong", input: strings.Repeat("S", maxStreamPreamble+1) + "\n", code: ErrBadArgs},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			r := strings.NewReader(tc.input)
			got, err := ReadStreamPreamble(r)

			switch {
			case tc.code != "":
				var de *DecodeError
				if !errors.As(err, &de) {
					t.Fatalf("ReadStreamPreamble error = %v; want *DecodeError", err)
				}
				if de.Code != tc.code {
					t.Fatalf("code = %q; want %q", de.Code, tc.code)
				}
			case tc.err != nil:
				if !errors.Is(err, tc.err) {
					t.Fatalf("error = %v; want %v", err, tc.err)
				}
			default:
				if err != nil {
					t.Fatalf("unexpected error: %v", err)
				}
				if got != tc.want {
					t.Fatalf("frame = %+v; want %+v", got, tc.want)
				}
				rest, _ := io.ReadAll(r)
				if string(rest) != tc.rest {
					t.Fatalf("rest = %q; want %q", rest, tc.rest)
				}
			}
		})
	}
}
