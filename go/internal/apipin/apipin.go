// Package apipin loads the cross-language contract files in api/ for tests.
package apipin

import (
	"bytes"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

// Read returns api/name, failing t when it is missing.
func Read(t testing.TB, name string) []byte {
	t.Helper()
	_, self, _, _ := runtime.Caller(0)
	data, err := os.ReadFile(filepath.Join(filepath.Dir(self), "..", "..", "..", "api", name))
	if err != nil {
		t.Fatal(err)
	}
	return data
}

// Rows returns a pipe-separated pin's rows as trimmed fields, failing t on a row without exactly n of them.
func Rows(t testing.TB, name string, n int) [][]string {
	t.Helper()
	var rows [][]string
	for line := range strings.SplitSeq(string(Read(t, name)), "\n") {
		if line = strings.TrimSpace(line); line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, "|")
		if len(fields) != n {
			t.Fatalf("%s: want %d fields in %q", name, n, line)
		}
		for i := range fields {
			fields[i] = strings.TrimSpace(fields[i])
		}
		rows = append(rows, fields)
	}
	return rows
}

// Schema compiles api/name.schema.json.
func Schema(t testing.TB, name string) *jsonschema.Schema {
	t.Helper()
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(Read(t, name+".schema.json")))
	if err != nil {
		t.Fatal(err)
	}
	c := jsonschema.NewCompiler()
	if err := c.AddResource(name+".schema.json", doc); err != nil {
		t.Fatal(err)
	}
	s, err := c.Compile(name + ".schema.json")
	if err != nil {
		t.Fatal(err)
	}
	return s
}

// Validate fails t unless data is JSON that s accepts.
func Validate(t testing.TB, s *jsonschema.Schema, data []byte) {
	t.Helper()
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(data))
	if err != nil {
		t.Fatalf("parse document: %v\n%s", err, data)
	}
	if err := s.Validate(doc); err != nil {
		t.Fatalf("schema validation: %v\n%s", err, data)
	}
}
