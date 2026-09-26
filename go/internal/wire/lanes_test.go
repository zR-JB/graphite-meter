package wire

import (
	"os"
	"strconv"
	"strings"
	"testing"
)

func TestLaneEndsMatchThePin(t *testing.T) {
	t.Parallel()
	raw, err := os.ReadFile("../../../api/laneendings.txt")
	if err != nil {
		t.Fatal(err)
	}
	var pinned []LaneEnd
	for line := range strings.SplitSeq(string(raw), "\n") {
		if line = strings.TrimSpace(line); line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, "|")
		if len(fields) != 4 {
			t.Fatalf("want 4 fields: %q", line)
		}
		ws, _ := strconv.Atoi(strings.TrimSpace(fields[1]))
		wt, _ := strconv.ParseUint(strings.TrimSpace(fields[2]), 10, 32)
		pinned = append(pinned, LaneEnd{strings.TrimSpace(fields[0]), ws, uint32(wt), strings.TrimSpace(fields[3])})
	}
	if len(pinned) != len(LaneEnds) {
		t.Fatalf("%d endings pinned, wire has %d", len(pinned), len(LaneEnds))
	}
	for i, end := range LaneEnds {
		if pinned[i] != end {
			t.Errorf("ending %d is %+v, pinned as %+v", i, end, pinned[i])
		}
	}
}
