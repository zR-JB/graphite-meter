package endpoint

import (
	"net/http/httptest"
	"os"
	"strconv"
	"strings"
	"testing"
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
		writeUploadAccessError(rec, access)
		if access == uploadAccessOK || rec.Code != status || strings.TrimSpace(rec.Body.String()) != message ||
			rec.Header().Get("X-Graphite-Upload-Refusal") != name {
			t.Errorf("%s answers %d %q, pinned as %d %q", name, rec.Code, rec.Body.String(), status, message)
		}
		pinned++
	}
	if pinned != len(uploadAccessInfos)-1 {
		t.Errorf("%d refusals pinned, Go produces %d", pinned, len(uploadAccessInfos)-1)
	}
}
