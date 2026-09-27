package wire

import (
	"strconv"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
)

func TestLaneEndsMatchThePin(t *testing.T) {
	t.Parallel()
	var pinned []LaneEnd
	for _, row := range apipin.Rows(t, "laneendings.txt", 4) {
		ws, _ := strconv.Atoi(row[1])
		wt, _ := strconv.ParseUint(row[2], 10, 32)
		pinned = append(pinned, LaneEnd{row[0], ws, uint32(wt), row[3]})
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
