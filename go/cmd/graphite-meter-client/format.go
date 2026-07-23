package main

import (
	"fmt"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

func targetChoiceLabel(target string) string {
	labels := map[string]string{
		"auto":           "Automatic",
		"http1-clear":    "HTTP/1.1 · clear",
		"http1-tls":      "HTTP/1.1 · TLS",
		"http2":          "HTTP/2 · TLS",
		"http3":          "HTTP/3 · QUIC",
		"ws-http1-clear": "WebSocket · HTTP/1.1 · clear",
		"ws-http1-tls":   "WebSocket · HTTP/1.1 · TLS",
	}
	if label := labels[target]; label != "" {
		return label
	}
	return target
}

func stageSummary(s goclient.StageSet) string {
	var parts []string
	if s.Latency {
		parts = append(parts, "latency")
	}
	if s.Download {
		parts = append(parts, "download")
	}
	if s.Upload {
		parts = append(parts, "upload")
	}
	if s.Bidirectional {
		parts = append(parts, "bidirectional")
	}
	if len(parts) == 0 {
		return "none"
	}
	return strings.Join(parts, ", ")
}

// runOrder previews the run screen's timeline: the same stages, in the same
// order, with the window each one measures for.
func runOrder(cfg goclient.Config) []string {
	stages := plannedStages(cfg)
	if len(stages) == 0 {
		return []string{warnStyle.Render("No stages selected")}
	}
	lines := make([]string, 0, len(stages))
	for _, s := range stages {
		lines = append(lines, fmt.Sprintf("%-14s %s", s.name, mutedStyle.Render(s.duration.String())))
	}
	return lines
}

func toggleLine(label string, on bool, note string) string {
	return fmt.Sprintf("%s %-22s %s", checkbox(on), label, mutedStyle.Render(note))
}

func valueLine(label, value, note string) string {
	return fmt.Sprintf("%-24s %s  %s", label, valueStyle.Render(value), mutedStyle.Render(note))
}

func checkbox(on bool) string {
	if on {
		return accentStyle.Render("●")
	}
	return mutedStyle.Render("○")
}

func timingLabel(row int) string {
	switch row {
	case 0:
		return "Warmup"
	case 1:
		return "Latency duration"
	case 2:
		return "Download duration"
	case 3:
		return "Upload duration"
	case 4:
		return "Bidirectional duration"
	case 5:
		return "Ping interval"
	default:
		return ""
	}
}

// renderBar fills width cells with value/scale. Every bar on a screen shares one
// scale — the largest value in view — so the fills compare against each other at
// a glance instead of each bar being full against its own maximum. lead brightens
// the last filled cell, which makes a live bar's motion legible.
func renderBar(value, scale float64, width int, lead bool) string {
	n := 0
	if scale > 0 {
		n = clamp(int((value/scale)*float64(width)+0.5), 0, width)
	}
	if n == 0 {
		return mutedStyle.Render(strings.Repeat("░", width))
	}
	filled := accentStyle.Render(strings.Repeat("█", n))
	if lead {
		filled = accentStyle.Render(strings.Repeat("█", n-1)) + valueStyle.Render("█")
	}
	return filled + mutedStyle.Render(strings.Repeat("░", width-n))
}

func rateLine(name string, rate, scale float64, w int) string {
	bar := renderBar(rate, scale, max(12, w-34), true)
	return fmt.Sprintf("%s %s %12s", labelStyle.Render(name), bar, valueStyle.Render(fmtRate(rate)))
}

// latencyLine separates the two reasons there is no round trip to show: no
// pong has come back yet, and a pong that never came back at all.
func latencyLine(s goclient.LatencySample) string {
	if s.Lost {
		return labelStyle.Render("latency ") + errorStyle.Render("timeout")
	}
	if s.RTT <= 0 {
		return labelStyle.Render("latency ") + mutedStyle.Render("waiting")
	}
	load := ""
	if s.UnderLoad {
		load = " loaded"
	}
	return labelStyle.Render("latency ") + valueStyle.Render(fmtMs(s.RTT)) + mutedStyle.Render(load)
}

func emptyDash(s string) string {
	if s == "" {
		return "--"
	}
	return s
}

func fmtRate(bytesPerSec float64) string {
	bits := bytesPerSec * 8
	units := []string{"bit/s", "Kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"}
	i := 0
	for bits >= 1000 && i < len(units)-1 {
		bits /= 1000
		i++
	}
	if i == 0 {
		return fmt.Sprintf("%.0f %s", bits, units[i])
	}
	return fmt.Sprintf("%.2f %s", bits, units[i])
}

func fmtBytes(n uint64) string {
	v := float64(n)
	units := []string{"B", "KB", "MB", "GB", "TB"}
	i := 0
	for v >= 1000 && i < len(units)-1 {
		v /= 1000
		i++
	}
	if i == 0 {
		return fmt.Sprintf("%d %s", n, units[i])
	}
	return fmt.Sprintf("%.2f %s", v, units[i])
}

// fmtClock renders a running clock: tenths while the eye can follow them,
// whole seconds once the number is long enough that they only flicker.
func fmtClock(d time.Duration) string {
	if d < 0 {
		d = 0
	}
	if d < time.Minute {
		return fmt.Sprintf("%.1fs", d.Seconds())
	}
	return d.Round(time.Second).String()
}

func fmtMs(d time.Duration) string {
	if d <= 0 {
		return "--"
	}
	return fmt.Sprintf("%.2f ms", float64(d.Microseconds())/1000)
}

func clamp(v, min, max int) int {
	if max < min {
		return min
	}
	if v < min {
		return min
	}
	if v > max {
		return max
	}
	return v
}

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}
