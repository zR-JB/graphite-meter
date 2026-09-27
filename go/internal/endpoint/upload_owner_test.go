package endpoint

import (
	"net/http/httptest"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Lane endings in the pin are checked against the server's real answers in server/lanes_test.go and
// server/revocation_test.go; every other row is an access refusal.
func TestUploadRefusalsMatchPin(t *testing.T) {
	pinned := 0
	for _, row := range apipin.Rows(t, "uploadrefusals.txt", 3) {
		name, message, status := row[0], row[1], row[2]
		if slices.ContainsFunc(wire.LaneEnds, func(e wire.LaneEnd) bool { return e.Name == name }) {
			continue
		}
		access := uploadAccessOK
		for a := range uploadAccessInfos {
			if uploadAccessInfos[a].code == name {
				access = uploadAccess(a)
			}
		}
		if access == uploadAccessOK {
			t.Errorf("%s is no refusal the server sends", name)
			continue
		}
		pinned++
		rec := httptest.NewRecorder()
		writeUploadAccessError(rec, access)
		if strconv.Itoa(rec.Code) != status || strings.TrimSpace(rec.Body.String()) != message ||
			rec.Header().Get("X-Graphite-Upload-Refusal") != name {
			t.Errorf("%s answers %d %q, pinned as %s %q", name, rec.Code, rec.Body.String(), status, message)
		}
	}
	if pinned != len(uploadAccessInfos)-1 {
		t.Errorf("%d refusals pinned, Go produces %d", pinned, len(uploadAccessInfos)-1)
	}
}
