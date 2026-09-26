package endpoint

import (
	"bufio"
	"maps"
	"net/http/httptest"
	"os"
	"strconv"
	"strings"
	"testing"
)

const refusalPinPath = "../../../api/uploadrefusals.txt"

// pinnedRefusal is one row of api/uploadrefusals.txt: the text a refused upload reports and the HTTP status it is sent.
type pinnedRefusal struct {
	message string
	status  int
}

// loadRefusalPin parses api/uploadrefusals.txt into name → refusal, skipping comment/blank lines.
func loadRefusalPin(t *testing.T) map[string]pinnedRefusal {
	t.Helper()
	f, err := os.Open(refusalPinPath)
	if err != nil {
		t.Fatalf("open refusal pin: %v", err)
	}
	defer f.Close()

	pinned := make(map[string]pinnedRefusal)
	scanner := bufio.NewScanner(f)
	lineNumber := 0
	for scanner.Scan() {
		lineNumber++
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, "|")
		if len(fields) != 3 {
			t.Fatalf("line %d: want 3 fields: %q", lineNumber, line)
		}
		status, err := strconv.Atoi(strings.TrimSpace(fields[2]))
		if err != nil {
			t.Fatalf("line %d: status %q is not a number", lineNumber, strings.TrimSpace(fields[2]))
		}
		pinned[strings.TrimSpace(fields[0])] = pinnedRefusal{message: strings.TrimSpace(fields[1]), status: status}
	}
	if err := scanner.Err(); err != nil {
		t.Fatalf("scan refusal pin: %v", err)
	}
	if len(pinned) == 0 {
		t.Fatal("refusal pin is empty: expected populated refusals")
	}
	return pinned
}

func TestUploadRefusalsMatchPin(t *testing.T) {
	pinned := loadRefusalPin(t)

	// Every refusal Go can produce, named as the pin names it.
	refusals := map[string]uploadAccess{
		"invalid":       uploadAccessInvalid,
		"globalFull":    uploadAccessGlobalFull,
		"clientFull":    uploadAccessClientFull,
		"ownerMismatch": uploadAccessOwnerMismatch,
	}
	if len(refusals) != len(pinned) {
		t.Errorf("Go declares %d refusals; %d are pinned", len(refusals), len(pinned))
	}
	for name, access := range refusals {
		want, ok := pinned[name]
		if !ok {
			t.Errorf("%s: declared in Go but not pinned", name)
			continue
		}
		if got := uploadAccessMessage(access); got != want.message {
			t.Errorf("%s: Go reports %q; pinned as %q", name, got, want.message)
		}
		rec := httptest.NewRecorder()
		writeUploadAccessError(rec, access)
		if rec.Code != want.status {
			t.Errorf("%s: Go answers %d; pinned as %d", name, rec.Code, want.status)
		}
		if got := strings.TrimSpace(rec.Body.String()); got != want.message {
			t.Errorf("%s: HTTP body = %q; pinned as %q", name, got, want.message)
		}
		if got := rec.Header().Get("X-Graphite-Upload-Refusal"); got != name {
			t.Errorf("%s: refusal header = %q, want %q", name, got, name)
		}
	}
	for name := range maps.Keys(pinned) {
		if _, ok := refusals[name]; !ok {
			t.Errorf("%s is pinned but Go never produces it", name)
		}
	}
}
