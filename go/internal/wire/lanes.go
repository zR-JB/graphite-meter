package wire

// LaneEnd is how a server tells its peer why a lane ended, on each transport (api/laneendings.txt).
type LaneEnd struct {
	Name   string
	WS     int
	WT     uint32
	Reason string
}

var (
	LaneFinished = LaneEnd{"finished", 1000, 0, ""}
	LaneIdle     = LaneEnd{"idle", 4001, 1, "idle"}
	LaneLifetime = LaneEnd{"lifetime", 4002, 2, "lifetime"}
	LaneRevoked  = LaneEnd{"revoked", 1008, 3, "authentication required"}
	LaneShutdown = LaneEnd{"shutdown", 1001, 4, "shutdown"}

	LaneEnds = []LaneEnd{LaneFinished, LaneIdle, LaneLifetime, LaneRevoked, LaneShutdown}
)
