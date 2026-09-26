package goclient

import (
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestSelectedStreamBudgets(t *testing.T) {
	t.Parallel()
	cfg := DefaultConfig()
	servers := []PreparedServer{}
	for _, id := range []string{"a", "b"} {
		servers = append(servers, PreparedServer{
			Server: wire.ServerEntry{ID: id},
			Connection: &PreparedConnection{
				ThroughputTarget: wire.ThroughputTarget{
					Origin:    "http://shared.example",
					Protocol:  "http1",
					Transport: wire.TransportFetchStream,
				},
			},
		})
	}
	plan, err := planRunStreams(cfg, servers)
	if err != nil || plan["a"] != (byDirection[int]{6, 6}) || plan["b"] != (byDirection[int]{6, 6}) {
		t.Fatalf("a shared HTTP/1 origin rationed native lanes: %v %v", plan, err)
	}
	cfg.TransferStreams.Forced = 65
	for i := range servers {
		servers[i].Connection.ThroughputTarget.Protocol = "http2"
	}
	if _, err := planRunStreams(cfg, servers); err == nil {
		t.Fatal("run-wide stream ceiling ignored")
	}
}
