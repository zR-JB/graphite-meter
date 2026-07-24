package goclient

import (
	"fmt"
	"time"
)

type StageSet struct {
	Latency       bool
	Download      bool
	Upload        bool
	Bidirectional bool
}

type TransferStreamPolicy struct {
	AutomaticMax int
	// Forced is exact per active direction. Zero selects automatic.
	Forced int
}

const (
	defaultAutomaticStreams = 6
	defaultH3Streams        = 3
	maxTransferStreams      = 128

	defaultLatencyDuration = 4 * time.Second
	// defaultTransferDuration covers the download, upload, and bidirectional
	// stages, which run the same window.
	defaultTransferDuration = 10 * time.Second
)

func (p TransferStreamPolicy) Resolve(protocol string) int {
	if p.Forced > 0 {
		return p.Forced
	}
	if protocol == "http3" {
		return defaultH3Streams
	}
	if protocol == "http2" {
		return 1
	}
	return p.AutomaticMax
}

func (p TransferStreamPolicy) Label(protocol string) string {
	if p.Forced > 0 {
		return fmt.Sprintf("Forced · %d per direction", p.Forced)
	}
	if protocol == "http3" {
		return fmt.Sprintf("Automatic · %d per direction", defaultH3Streams)
	}
	if protocol == "http2" {
		return "Automatic · 1 per direction"
	}
	if protocol == "http1" || protocol == "http1-clear" || protocol == "http1-tls" {
		return fmt.Sprintf("Automatic · up to %d per direction", p.AutomaticMax)
	}
	return "Automatic"
}

type Config struct {
	BaseURL                string
	ThroughputTarget       string
	ThroughputProtocol     string
	ThroughputTransport    string
	LatencyTarget          string
	LatencyTransport       string
	Stages                 StageSet
	Warmup                 time.Duration
	LatencyDuration        time.Duration
	DownloadDuration       time.Duration
	UploadDuration         time.Duration
	BidirectionalDuration  time.Duration
	TransferStreams        TransferStreamPolicy
	PingInterval           time.Duration
	LoadedLatency          bool
	DownloadBytesPerStream int64
	UploadBytesPerStream   int64
	InsecureSkipTLSVerify  bool
	AuthToken              string
	AuthOrigin             string
	MaxIdleConnsPerHost    int
	ResponseHeaderTimeout  time.Duration
	ExpectContinueTimeout  time.Duration
}

func DefaultConfig() Config {
	return Config{
		BaseURL:                "http://127.0.0.1:7246",
		ThroughputTarget:       "auto",
		ThroughputProtocol:     "auto",
		ThroughputTransport:    "auto",
		LatencyTarget:          "auto",
		LatencyTransport:       "auto",
		Stages:                 StageSet{Latency: true, Download: true, Upload: true},
		Warmup:                 800 * time.Millisecond,
		LatencyDuration:        defaultLatencyDuration,
		DownloadDuration:       defaultTransferDuration,
		UploadDuration:         defaultTransferDuration,
		BidirectionalDuration:  defaultTransferDuration,
		TransferStreams:        TransferStreamPolicy{AutomaticMax: defaultAutomaticStreams},
		PingInterval:           250 * time.Millisecond,
		LoadedLatency:          true,
		DownloadBytesPerStream: 64 * 1024 * 1024 * 1024,
		UploadBytesPerStream:   64 * 1024 * 1024 * 1024,
		MaxIdleConnsPerHost:    256,
		ResponseHeaderTimeout:  10 * time.Second,
		ExpectContinueTimeout:  time.Second,
	}
}

func (c Config) normalized() Config {
	if c.BaseURL == "" {
		c.BaseURL = "http://127.0.0.1:7246"
	}
	if c.ThroughputTarget == "" {
		c.ThroughputTarget = "auto"
	}
	if c.ThroughputProtocol == "" {
		c.ThroughputProtocol = "auto"
	}
	if c.ThroughputTransport == "" {
		c.ThroughputTransport = "auto"
	}
	if c.LatencyTarget == "" {
		c.LatencyTarget = "auto"
	}
	if c.LatencyTransport == "" {
		c.LatencyTransport = "auto"
	}
	if c.Warmup < 0 {
		c.Warmup = 0
	}
	if c.LatencyDuration <= 0 {
		c.LatencyDuration = defaultLatencyDuration
	}
	if c.DownloadDuration <= 0 {
		c.DownloadDuration = defaultTransferDuration
	}
	if c.UploadDuration <= 0 {
		c.UploadDuration = defaultTransferDuration
	}
	if c.BidirectionalDuration <= 0 {
		c.BidirectionalDuration = defaultTransferDuration
	}
	if c.TransferStreams.Forced < 0 {
		c.TransferStreams.Forced = 0
	}
	if c.TransferStreams.AutomaticMax < 1 {
		c.TransferStreams.AutomaticMax = defaultAutomaticStreams
	}
	if c.TransferStreams.AutomaticMax > maxTransferStreams {
		c.TransferStreams.AutomaticMax = maxTransferStreams
	}
	if c.TransferStreams.Forced > maxTransferStreams {
		c.TransferStreams.Forced = maxTransferStreams
	}
	if c.PingInterval <= 0 {
		c.PingInterval = 250 * time.Millisecond
	}
	if c.DownloadBytesPerStream <= 0 {
		c.DownloadBytesPerStream = 64 * 1024 * 1024 * 1024
	}
	if c.UploadBytesPerStream <= 0 {
		c.UploadBytesPerStream = 64 * 1024 * 1024 * 1024
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
