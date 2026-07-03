package goclient

import "time"

type StageSet struct {
	Latency       bool
	Download      bool
	Upload        bool
	Bidirectional bool
}

type Config struct {
	BaseURL                string
	Stages                 StageSet
	Warmup                 time.Duration
	LatencyDuration        time.Duration
	DownloadDuration       time.Duration
	UploadDuration         time.Duration
	BidirectionalDuration  time.Duration
	ParallelStreams        int
	PingInterval           time.Duration
	LoadedLatency          bool
	DownloadBytesPerStream int64
	UploadProgressSettle   time.Duration
	InsecureSkipTLSVerify  bool
	MaxIdleConnsPerHost    int
	ResponseHeaderTimeout  time.Duration
	ExpectContinueTimeout  time.Duration
}

func DefaultConfig() Config {
	return Config{
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
		UploadProgressSettle:   250 * time.Millisecond,
		MaxIdleConnsPerHost:    256,
		ResponseHeaderTimeout:  10 * time.Second,
		ExpectContinueTimeout:  time.Second,
	}
}

func (c Config) normalized() Config {
	if c.BaseURL == "" {
		c.BaseURL = "http://127.0.0.1:8765"
	}
	if c.Warmup < 0 {
		c.Warmup = 0
	}
	if c.LatencyDuration <= 0 {
		c.LatencyDuration = 4 * time.Second
	}
	if c.DownloadDuration <= 0 {
		c.DownloadDuration = 10 * time.Second
	}
	if c.UploadDuration <= 0 {
		c.UploadDuration = 10 * time.Second
	}
	if c.BidirectionalDuration <= 0 {
		c.BidirectionalDuration = 10 * time.Second
	}
	if c.ParallelStreams < 1 {
		c.ParallelStreams = 1
	}
	if c.ParallelStreams > 128 {
		c.ParallelStreams = 128
	}
	if c.PingInterval <= 0 {
		c.PingInterval = 250 * time.Millisecond
	}
	if c.DownloadBytesPerStream <= 0 {
		c.DownloadBytesPerStream = 64 * 1024 * 1024 * 1024
	}
	if c.UploadProgressSettle <= 0 {
		c.UploadProgressSettle = 250 * time.Millisecond
	}
	if c.MaxIdleConnsPerHost <= 0 {
		c.MaxIdleConnsPerHost = 256
	}
	if c.ResponseHeaderTimeout <= 0 {
		c.ResponseHeaderTimeout = 10 * time.Second
	}
	if c.ExpectContinueTimeout <= 0 {
		c.ExpectContinueTimeout = time.Second
	}
	return c
}
