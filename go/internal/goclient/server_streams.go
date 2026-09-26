package goclient

import "fmt"

// planRunStreams gives each server its policy's lanes; the run owns only the shared per-direction ceiling.
// Native transports have no browser-style six-connection origin limit, so HTTP/1 lanes are not rationed.
func planRunStreams(cfg Config, servers []PreparedServer) (map[string]streamCounts, error) {
	plan := map[string]streamCounts{}
	var total streamCounts
	for _, server := range servers {
		if server.Connection == nil {
			return nil, fmt.Errorf("%s is not ready", server.Server.Name)
		}
		target := server.Connection.ThroughputTarget
		lanes := cfg.TransferStreams.lanes(target.Protocol, target.Transport)
		plan[server.Server.ID] = lanes
		total.down += lanes.down
		total.up += lanes.up
	}
	if total.down > maxTransferStreams || total.up > maxTransferStreams {
		return nil, fmt.Errorf("the run exceeds %d streams per direction; reduce forced streams", maxTransferStreams)
	}
	return plan, nil
}
