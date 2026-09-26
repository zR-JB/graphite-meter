package endpoint

import (
	"net/http/httptest"
	"os"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestUploadRefusalsMatchPin(t *testing.T) {
	raw, err := os.ReadFile("../../../api/uploadrefusals.txt")
	if err != nil {
		t.Fatal(err)
	}
	pinned := 0
	for line := range strings.SplitSeq(string(raw), "\n") {
		if line = strings.TrimSpace(line); line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, "|")
		if len(fields) != 3 {
			t.Fatalf("want 3 fields: %q", line)
		}
		name, message := strings.TrimSpace(fields[0]), strings.TrimSpace(fields[1])
		status, _ := strconv.Atoi(strings.TrimSpace(fields[2]))
		access := uploadAccessOK
		for a := range uploadAccessInfos {
			if uploadAccessInfos[a].code == name {
				access = uploadAccess(a)
			}
		}
		rec := httptest.NewRecorder()
		switch end := slices.IndexFunc(wire.LaneEnds, func(e wire.LaneEnd) bool { return e.Name == name }); {
		case end >= 0:
			writeLaneRefusal(rec, wire.LaneEnds[end])
		case access == uploadAccessOK:
			t.Errorf("%s is no refusal the server sends", name)
			continue
		default:
			writeUploadAccessError(rec, access)
			pinned++
		}
		if rec.Code != status || strings.TrimSpace(rec.Body.String()) != message ||
			rec.Header().Get("X-Graphite-Upload-Refusal") != name {
			t.Errorf("%s answers %d %q, pinned as %d %q", name, rec.Code, rec.Body.String(), status, message)
		}
	}
	if pinned != len(uploadAccessInfos)-1 {
		t.Errorf("%d refusals pinned, Go produces %d", pinned, len(uploadAccessInfos)-1)
	}
}
