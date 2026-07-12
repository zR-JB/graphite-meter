package goclient

import (
	"reflect"
	"testing"
	"time"
)

func TestDefaultConfig(t *testing.T) {
	want := Config{
		BaseURL:                "http://127.0.0.1:8765",
		Stages:                 StageSet{Latency: true, Download: true, Upload: true},
		Warmup:                 800 * time.Millisecond,
		LatencyDuration:        4 * time.Second,
		DownloadDuration:       10 * time.Second,
		UploadDuration:         10 * time.Second,
		BidirectionalDuration:  10 * time.Second,
		ParallelStreams:        4,
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
			name:   "ParallelStreams below 1 clamps to 1",
			mutate: func(c Config) Config { c.ParallelStreams = 0; return c },
			check:  func(c Config) (any, any) { return c.ParallelStreams, 1 },
		},
		{
			name:   "ParallelStreams negative clamps to 1",
			mutate: func(c Config) Config { c.ParallelStreams = -5; return c },
			check:  func(c Config) (any, any) { return c.ParallelStreams, 1 },
		},
		{
			name:   "ParallelStreams above 128 clamps to 128",
			mutate: func(c Config) Config { c.ParallelStreams = 500; return c },
			check:  func(c Config) (any, any) { return c.ParallelStreams, 128 },
		},
		{
			name:   "ParallelStreams in range passes through",
			mutate: func(c Config) Config { c.ParallelStreams = 64; return c },
			check:  func(c Config) (any, any) { return c.ParallelStreams, 64 },
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
