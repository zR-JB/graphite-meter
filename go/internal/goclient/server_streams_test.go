package goclient

import (
	"github.com/zR-JB/graphite-meter/go/internal/wire"
	"testing"
)

func TestSelectedStreamBudgets(t *testing.T) {
	cfg := DefaultConfig()
	cfg.Stages = StageSet{Upload: true}
	cfg.LoadedLatency = false
	servers := []PreparedServer{}
	for _, id := range []string{"a", "b"} {
		servers = append(servers, PreparedServer{Server: wire.ServerEntry{ID: id}, Connection: &PreparedConnection{ThroughputTarget: wire.ThroughputTarget{Origin: "http://shared.example", Protocol: "http1", Transport: wire.TransportFetchStream}}})
	}
	plans, err := planRunStreams(cfg, servers)
	if err != nil || plans["upload"]["a"].up+plans["upload"]["b"].up != 3 {
		t.Fatalf("shared origin control reservation: %v %v", plans, err)
	}
	cfg.TransferStreams.Forced = 3
	if _, err := planRunStreams(cfg, servers); err == nil {
		t.Fatal("forced streams stole control capacity")
	}
	cfg.TransferStreams.Forced = 65
	for i := range servers {
		servers[i].Connection.ThroughputTarget.Protocol = "http2"
	}
	if _, err := planRunStreams(cfg, servers); err == nil {
		t.Fatal("run-wide stream ceiling ignored")
	}
}
