// Checks the Go<->Rust QPACK contract with the repository's unchanged quic-go/qpack: each block's
// "go:" line must be Go's encoding of its fields, and Go must decode the "rust:" line to exactly
// those fields. Vectors are read from standard input.
package main

import (
	"bytes"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"

	"github.com/quic-go/qpack"
)

func main() {
	if err := run(os.Stdin); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(input io.Reader) error {
	text, err := io.ReadAll(input)
	if err != nil {
		return err
	}
	var fields []qpack.HeaderField
	var problems []string
	for number, line := range strings.Split(string(text), "\n") {
		where := fmt.Sprintf("vectors:%d", number+1)
		switch {
		case strings.HasPrefix(line, "go: "):
			if encoded := encode(fields); line != "go: "+encoded {
				problems = append(problems, where+": Go encodes the block as "+encoded)
			}
		case strings.HasPrefix(line, "rust: "):
			if err := decodes(strings.TrimPrefix(line, "rust: "), fields); err != nil {
				problems = append(problems, where+": "+err.Error())
			}
			fields = nil
		case line != "" && !strings.HasPrefix(line, "#"):
			name, value, _ := strings.Cut(line, " ")
			field, err := unescape(name, value)
			if err != nil {
				return fmt.Errorf("%s: %w", where, err)
			}
			fields = append(fields, field)
		}
	}
	if len(problems) > 0 {
		return errors.New(strings.Join(problems, "\n"))
	}
	return nil
}

func encode(fields []qpack.HeaderField) string {
	var buffer bytes.Buffer
	encoder := qpack.NewEncoder(&buffer)
	for _, field := range fields {
		if err := encoder.WriteField(field); err != nil {
			panic(err)
		}
	}
	return hex.EncodeToString(buffer.Bytes())
}

func decodes(encoded string, want []qpack.HeaderField) error {
	section, err := hex.DecodeString(encoded)
	if err != nil {
		return err
	}
	decode := qpack.NewDecoder().Decode(section)
	var got []qpack.HeaderField
	for {
		field, err := decode()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return fmt.Errorf("Go cannot decode the Rust encoding: %w", err)
		}
		got = append(got, field)
	}
	if fmt.Sprint(got) != fmt.Sprint(want) {
		return fmt.Errorf("Go decodes the Rust encoding as %q, want %q", got, want)
	}
	return nil
}

func unescape(name, value string) (qpack.HeaderField, error) {
	var field [2]string
	for index, text := range []string{name, value} {
		var decoded strings.Builder
		for position := 0; position < len(text); position++ {
			switch {
			case text[position] != '\\':
				decoded.WriteByte(text[position])
			case strings.HasPrefix(text[position:], `\\`):
				decoded.WriteByte('\\')
				position++
			case strings.HasPrefix(text[position:], `\x`) && position+4 <= len(text):
				code, err := strconv.ParseUint(text[position+2:position+4], 16, 8)
				if err != nil {
					return qpack.HeaderField{}, err
				}
				decoded.WriteByte(byte(code))
				position += 3
			default:
				return qpack.HeaderField{}, fmt.Errorf("bad escape in %q", text)
			}
		}
		field[index] = decoded.String()
	}
	return qpack.HeaderField{Name: field[0], Value: field[1]}, nil
}
