package goclient

import (
	"reflect"
	"testing"
	"time"
)

func TestDefaultConfig(t *testing.T) {
	want := Config{
		BaseURL:                "http://127.0.0.1:8765",
		ThroughputTarget:       "auto",
		LatencyTarget:          "auto",
		Stages:                 StageSet{Latency: true, Download: true, Upload: true},
		Warmup:                 800 * time.Millisecond,
		LatencyDuration:        4 * time.Second,
		DownloadDuration:       10 * time.Second,
		UploadDuration:         10 * time.Second,
		BidirectionalDuration:  10 * time.Second,
		TransferStreams:        TransferStreamPolicy{AutomaticMax: 6},
		PingInterval:           250 * time.Millisecond,
		LoadedLatency:          true,
		DownloadBytesPerStream: 64 * 1024 * 1024 * 1024,
		UploadBytesPerStream:   64 * 1024 * 1024 * 1024,
		MaxIdleConnsPerHost:    256,
		ResponseHeaderTimeout:  10 * time.Second,
		ExpectContinueTimeout:  time.Second,
	}
	if got := DefaultConfig(); !reflect.DeepEqual(got, want) {
		t.Errorf("DefaultConfig() = %+v, want %+v", got, want)
	}
}

func TestTransferStreamPolicy(t *testing.T) {
	auto := TransferStreamPolicy{AutomaticMax: 6}
	if got := auto.Resolve("http1"); got != 6 {
		t.Fatalf("automatic HTTP/1 streams = %d, want 6", got)
	}
	for _, protocol := range []string{"http2", "http3"} {
		if got := auto.Resolve(protocol); got != 1 {
			t.Errorf("automatic %s streams = %d, want 1", protocol, got)
		}
	}
	forced := TransferStreamPolicy{Forced: 9}
	for _, protocol := range []string{"http1", "http2", "http3"} {
		if got := forced.Resolve(protocol); got != 9 {
			t.Errorf("forced %s streams = %d, want 9", protocol, got)
		}
	}
	if got := forced.Label("http3"); got != "Forced · 9 per direction" {
		t.Errorf("forced label = %q", got)
	}
}

func TestConfigNormalized(t *testing.T) {
	base := DefaultConfig()

	cases := []struct {
		name   string
		mutate func(c Config) Config
		check  func(c Config) (got, want any)
	}{
		{
			name:   "empty BaseURL defaults",
			mutate: func(c Config) Config { c.BaseURL = ""; return c },
			check:  func(c Config) (any, any) { return c.BaseURL, "http://127.0.0.1:8765" },
		},
		{
			name:   "advertised throughput target id passes through",
			mutate: func(c Config) Config { c.ThroughputTarget = "edge-h2"; return c },
			check:  func(c Config) (any, any) { return c.ThroughputTarget, "edge-h2" },
		},
		{
			name:   "negative Warmup clamps to 0",
			mutate: func(c Config) Config { c.Warmup = -1 * time.Second; return c },
			check:  func(c Config) (any, any) { return c.Warmup, time.Duration(0) },
		},
		{
			name:   "zero LatencyDuration defaults",
			mutate: func(c Config) Config { c.LatencyDuration = 0; return c },
			check:  func(c Config) (any, any) { return c.LatencyDuration, 4 * time.Second },
		},
		{
			name:   "negative LatencyDuration defaults",
			mutate: func(c Config) Config { c.LatencyDuration = -1; return c },
			check:  func(c Config) (any, any) { return c.LatencyDuration, 4 * time.Second },
		},
		{
			name:   "zero DownloadDuration defaults",
			mutate: func(c Config) Config { c.DownloadDuration = 0; return c },
			check:  func(c Config) (any, any) { return c.DownloadDuration, 10 * time.Second },
		},
		{
			name:   "zero UploadDuration defaults",
			mutate: func(c Config) Config { c.UploadDuration = 0; return c },
			check:  func(c Config) (any, any) { return c.UploadDuration, 10 * time.Second },
		},
		{
			name:   "zero BidirectionalDuration defaults",
			mutate: func(c Config) Config { c.BidirectionalDuration = 0; return c },
			check:  func(c Config) (any, any) { return c.BidirectionalDuration, 10 * time.Second },
		},
		{
			name:   "zero automatic max restores default",
			mutate: func(c Config) Config { c.TransferStreams.AutomaticMax = 0; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.AutomaticMax, 6 },
		},
		{
			name:   "automatic max clamps to 128",
			mutate: func(c Config) Config { c.TransferStreams.AutomaticMax = 500; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.AutomaticMax, 128 },
		},
		{
			name:   "negative forced stream count selects automatic",
			mutate: func(c Config) Config { c.TransferStreams.Forced = -5; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.Forced, 0 },
		},
		{
			name:   "forced stream count clamps to 128",
			mutate: func(c Config) Config { c.TransferStreams.Forced = 500; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.Forced, 128 },
		},
		{
			name:   "forced stream count in range passes through",
			mutate: func(c Config) Config { c.TransferStreams.Forced = 64; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.Forced, 64 },
		},
		{
			name:   "zero PingInterval defaults",
			mutate: func(c Config) Config { c.PingInterval = 0; return c },
			check:  func(c Config) (any, any) { return c.PingInterval, 250 * time.Millisecond },
		},
		{
			name:   "zero DownloadBytesPerStream defaults",
			mutate: func(c Config) Config { c.DownloadBytesPerStream = 0; return c },
			check:  func(c Config) (any, any) { return c.DownloadBytesPerStream, int64(64 * 1024 * 1024 * 1024) },
		},
		{
			name:   "negative DownloadBytesPerStream defaults",
			mutate: func(c Config) Config { c.DownloadBytesPerStream = -1; return c },
			check:  func(c Config) (any, any) { return c.DownloadBytesPerStream, int64(64 * 1024 * 1024 * 1024) },
		},
		{
			name:   "zero UploadBytesPerStream defaults",
			mutate: func(c Config) Config { c.UploadBytesPerStream = 0; return c },
			check:  func(c Config) (any, any) { return c.UploadBytesPerStream, int64(64 * 1024 * 1024 * 1024) },
		},
		{
			name:   "negative UploadBytesPerStream defaults",
			mutate: func(c Config) Config { c.UploadBytesPerStream = -1; return c },
			check:  func(c Config) (any, any) { return c.UploadBytesPerStream, int64(64 * 1024 * 1024 * 1024) },
		},
		{
			name:   "zero MaxIdleConnsPerHost defaults",
			mutate: func(c Config) Config { c.MaxIdleConnsPerHost = 0; return c },
			check:  func(c Config) (any, any) { return c.MaxIdleConnsPerHost, 256 },
		},
		{
			name:   "zero ResponseHeaderTimeout defaults",
			mutate: func(c Config) Config { c.ResponseHeaderTimeout = 0; return c },
			check:  func(c Config) (any, any) { return c.ResponseHeaderTimeout, 10 * time.Second },
		},
		{
			name:   "zero ExpectContinueTimeout defaults",
			mutate: func(c Config) Config { c.ExpectContinueTimeout = 0; return c },
			check:  func(c Config) (any, any) { return c.ExpectContinueTimeout, time.Second },
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c := tc.mutate(base)
			got, want := tc.check(c.normalized())
			if got != want {
				t.Errorf("got %v, want %v", got, want)
			}
		})
	}
}
