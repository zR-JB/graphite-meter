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
	maxTransferStreams      = 128
)

func (p TransferStreamPolicy) Resolve(protocol string) int {
	if p.Forced > 0 {
		return p.Forced
	}
	if protocol == "http2" || protocol == "http3" {
		return 1
	}
	return p.AutomaticMax
}

func (p TransferStreamPolicy) Label(protocol string) string {
	if p.Forced > 0 {
		return fmt.Sprintf("Forced · %d per direction", p.Forced)
	}
	if protocol == "http2" || protocol == "http3" {
		return "Automatic · 1 per direction"
	}
	if protocol == "http1" {
		return fmt.Sprintf("Automatic · up to %d per direction", p.AutomaticMax)
	}
	return "Automatic"
}

type Config struct {
	BaseURL                string
	Protocol               string
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
	MaxIdleConnsPerHost    int
	ResponseHeaderTimeout  time.Duration
	ExpectContinueTimeout  time.Duration
}

func DefaultConfig() Config {
	return Config{
		BaseURL:                "http://127.0.0.1:8765",
		Protocol:               "auto",
		Stages:                 StageSet{Latency: true, Download: true, Upload: true},
		Warmup:                 800 * time.Millisecond,
		LatencyDuration:        4 * time.Second,
		DownloadDuration:       10 * time.Second,
		UploadDuration:         10 * time.Second,
		BidirectionalDuration:  10 * time.Second,
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
		c.BaseURL = "http://127.0.0.1:8765"
	}
	switch c.Protocol {
	case "", "auto":
		c.Protocol = "auto"
	case "http1", "http2", "http3":
	default:
		c.Protocol = "invalid"
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
