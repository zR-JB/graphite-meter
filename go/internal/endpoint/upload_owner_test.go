package endpoint

import (
	"bufio"
	"net/http"
	"net/http/httptest"
	"os"
	"strconv"
	"strings"
	"testing"
)

const refusalPinPath = "../../../api/uploadrefusals.txt"

// pinnedRefusal is one row of api/uploadrefusals.txt: the text a refused upload
// reports and the HTTP status it is sent with.
type pinnedRefusal struct {
	message string
	status  int
}

// loadRefusalPin parses api/uploadrefusals.txt into name → refusal, skipping
// comment/blank lines. This is the same fixture the TypeScript progress feed
// asserts its fatal-detail strings against.
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

// TestUploadRefusalsMatchPin is the byte-exact check both languages run against
// the same file. The message is not a diagnostic: a refused WebTransport lane
// gets no status line, so this exact text is the only thing the client can
// recognise the fatal by. Rewording it in Go alone would leave both suites green
// while the client stopped acting on the refusal.
func TestUploadRefusalsMatchPin(t *testing.T) {
	pinned := loadRefusalPin(t)

	// Every refusal Go can produce, named as the pin names it. uploadAccessOK is
	// not a refusal and is asserted separately below.
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
	for name := range pinned {
		if _, ok := refusals[name]; !ok {
			t.Errorf("%s is pinned but Go never produces it", name)
		}
	}
}

// A success carries no refusal text, so an accepted upload can never be mistaken
// for a refused one by a client matching on the message.
func TestUploadAccessOKCarriesNoMessage(t *testing.T) {
	if got := uploadAccessMessage(uploadAccessOK); got != "" {
		t.Errorf("uploadAccessMessage(uploadAccessOK) = %q, want empty", got)
	}
	if got := uploadAccessCode(uploadAccessOK); got != "" {
		t.Errorf("uploadAccessCode(uploadAccessOK) = %q, want empty", got)
	}
}

// writeUploadAccessError must never fall through silently. Without a default
// arm a future uploadAccess value writes no status and no body, Handle returns
// nil, and the client reads a 200 with an empty body as a successful upload
// reporting zero bytes.
func TestWriteUploadAccessErrorFailsLoudlyOnAnUnknownAccess(t *testing.T) {
	rec := httptest.NewRecorder()
	writeUploadAccessError(rec, uploadAccess(200))
	if rec.Code == http.StatusOK {
		t.Fatal("an unhandled refusal wrote no status: the client would read the 200 as a successful upload")
	}
	if rec.Code < 500 {
		t.Errorf("status = %d, want a 5xx: an unmapped refusal is a server bug, not the client's fault", rec.Code)
	}
	if strings.TrimSpace(rec.Body.String()) == "" {
		t.Error("an unhandled refusal wrote no body")
	}
}
