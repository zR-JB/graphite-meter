package goclient

import (
	"fmt"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// The run owns the shared budget; participants receive only their allotted data lanes.
func planRunStreams(cfg Config, servers []PreparedServer) (map[string]map[string]streamCounts, error) {
	plans := map[string]map[string]streamCounts{}
	for _, stage := range cfg.Plan() {
		counts := map[string]streamCounts{}
		type lane struct {
			id      string
			dir     Direction
			ceiling int
		}
		groups := map[string][]lane{}
		control := map[string]int{}
		for _, server := range servers {
			if server.Connection == nil {
				return nil, fmt.Errorf("%s is not ready", server.Server.Name)
			}
			target := server.Connection.ThroughputTarget
			all := cfg.TransferStreams.lanes(target.Protocol, target.Transport)
			var used streamCounts
			for _, dir := range stage.Directions {
				n := all.of(dir)
				if dir == Down {
					used.down = n
				} else {
					used.up = n
					if target.Transport == wire.TransportFetchStream {
						control[target.Origin]++
					}
				}
				if target.Transport == wire.TransportFetchStream && target.Protocol == "http1" {
					groups[target.Origin] = append(groups[target.Origin], lane{server.Server.ID, dir, n})
				}
			}
			counts[server.Server.ID] = used
			latency := server.Connection.LatencyTarget
			if len(stage.Directions) > 0 && cfg.LoadedLatency && latency != nil && latency.Transport == wire.TransportWebSocket {
				control[latency.Origin]++
			}
		}
		for origin, lanes := range groups {
			available := 6 - control[origin]
			if stage.Name == "upload" || stage.Name == "bidirectional" {
				available--
			}
			if available < len(lanes) {
				return nil, fmt.Errorf("selected servers share %s with insufficient HTTP/1 progress and control capacity", origin)
			}
			wanted := 0
			for _, lane := range lanes {
				wanted += lane.ceiling
			}
			if wanted <= available {
				continue
			}
			if cfg.TransferStreams.Forced > 0 {
				return nil, fmt.Errorf("forced streams at %s exceed shared HTTP/1 capacity; reduce streams or use Automatic", origin)
			}
			set := func(l lane, n int) {
				count := counts[l.id]
				if l.dir == Down {
					count.down = n
				} else {
					count.up = n
				}
				counts[l.id] = count
			}
			for _, lane := range lanes {
				set(lane, 1)
			}
			remaining := available - len(lanes)
			for remaining > 0 {
				assigned := false
				for _, lane := range lanes {
					n := counts[lane.id].of(lane.dir)
					if remaining > 0 && n < lane.ceiling {
						set(lane, n+1)
						remaining--
						assigned = true
					}
				}
				if !assigned {
					break
				}
			}
		}
		down, up := 0, 0
		for _, count := range counts {
			down += count.down
			up += count.up
		}
		if down > maxTransferStreams || up > maxTransferStreams {
			return nil, fmt.Errorf("the run exceeds %d streams per direction; reduce forced streams", maxTransferStreams)
		}
		plans[stage.Name] = counts
	}
	return plans, nil
}
