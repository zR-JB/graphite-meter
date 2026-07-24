package transport

import "testing"

func TestQUICConfigFitsMinimumTunnelMTU(t *testing.T) {
	cfg := NewQUICConfig()
	if cfg.InitialPacketSize != 1200 {
		t.Fatalf("initial packet size = %d, want 1200", cfg.InitialPacketSize)
	}
	if cfg.DisablePathMTUDiscovery {
		t.Fatal("path MTU discovery is disabled")
	}
	if !cfg.EnableDatagrams {
		t.Fatal("datagrams are disabled: WebTransport upgrade requires them")
	}
}
