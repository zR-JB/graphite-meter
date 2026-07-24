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
	if !cfg.EnableDatagrams || !cfg.EnableStreamResetPartialDelivery {
		t.Fatalf("config = %+v, want the WebTransport prerequisites enabled", cfg)
	}
	if cfg.MaxStreamReceiveWindow < 32<<20 || cfg.MaxConnectionReceiveWindow < cfg.MaxStreamReceiveWindow {
		t.Fatalf("receive windows = %d/%d, want a single stream to saturate a fat pipe", cfg.MaxStreamReceiveWindow, cfg.MaxConnectionReceiveWindow)
	}
}
