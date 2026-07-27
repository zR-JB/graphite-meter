package main

import (
	"fmt"
	"net/url"
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

// labelColumn is the width every menu row's label holds, so the values beside
// them line up down the panel. fieldColumn is the same idea for the readings
// on the run screen, whose labels are single words.
const (
	labelColumn = 22
	fieldColumn = 11
)

// field is one labelled reading. The value carries its own styling, so the
// label is padded before it is rendered rather than after.
func field(label, value string) string {
	return labelStyle.Render(pad(label, fieldColumn)) + value
}

func toggleLine(label string, on bool, note string) string {
	return fmt.Sprintf("%s %-*s %s", checkbox(on), labelColumn-2, label, mutedStyle.Render(note))
}

func valueLine(label, value, note string) string {
	return fmt.Sprintf("%-*s %s  %s", labelColumn, label, valueStyle.Render(value), mutedStyle.Render(note))
}

// inertValueLine is valueLine for a setting the rest of the configuration
// ignores: the value drops to the note's tone, so the row reads as inactive
// without moving or disappearing.
func inertValueLine(label, value, note string) string {
	return fmt.Sprintf("%s %s  %s", mutedStyle.Render(pad(label, labelColumn)), mutedStyle.Render(value), mutedStyle.Render(note))
}

// endpointRow is an endpoint selector line: what the configured choice is,
// where in the cycle enter walks it sits, and where it points. Both
// descriptions come from discovery, so the row stands still while a connection
// check runs.
func endpointRow(label, configured string, choices []endpointChoice) string {
	value, note, pos := targetChoiceLabel(configured), "", ""
	for i, c := range choices {
		if c.value == configured {
			value, note = c.label, c.note
			pos = fmt.Sprintf(" ‹%d/%d›", i+1, len(choices))
			break
		}
	}
	row := fmt.Sprintf("%-*s %s%s", labelColumn, label, valueStyle.Render(value), mutedStyle.Render(pos))
	if note != "" {
		row += mutedStyle.Render("  " + note)
	}
	return row
}

// protocolFacts is what selecting a target means: the protocol it fixes and
// whether it is encrypted. Discovery carries both for every origin it lists.
func protocolFacts(protocol string, tls bool) string {
	label := map[string]string{
		"http1":      "HTTP/1.1",
		"http2":      "HTTP/2",
		"http3":      "HTTP/3",
		"negotiated": "Negotiated",
	}[protocol]
	if label == "" {
		label = emptyDash(protocol)
	}
	if tls {
		return label + " · TLS"
	}
	return label + " · clear"
}

// shortOrigin names a target's origin against the server the client is talking
// to. Native listeners differ from that server by port alone, so the port on
// its own identifies them; anything else is named by host and port.
func shortOrigin(base, target string) string {
	u, err := url.Parse(target)
	if err != nil || u.Host == "" {
		return target
	}
	if b, err := url.Parse(base); err == nil && u.Port() != "" && strings.EqualFold(b.Hostname(), u.Hostname()) {
		return ":" + u.Port()
	}
	return u.Host
}

// pad widens s to at least w cells. Styling is applied after padding, so the
// padding is counted in cells rather than in escape bytes.
func pad(s string, w int) string {
	if len(s) >= w {
		return s
	}
	return s + strings.Repeat(" ", w-len(s))
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

// eighths are the partial-cell fills between an empty and a full block. A
// bar's tip moves in sub-cell steps.
var eighths = []string{"", "▏", "▎", "▍", "▌", "▋", "▊", "▉"}

// renderBar fills width cells with value/scale. Callers pass the largest value
// in view as scale, so every bar on a screen compares against one denominator.
// The tip renders at eighth-cell resolution. lead brightens the last filled
// cell, which makes a live bar's motion legible.
func renderBar(value, scale float64, width int, lead bool) string {
	cells := 0.0
	if scale > 0 {
		cells = value / scale * float64(width)
		if cells < 0 {
			cells = 0
		}
		if cells > float64(width) {
			cells = float64(width)
		}
	}
	full := int(cells)
	part := eighths[int((cells-float64(full))*8)]
	if full == 0 && part == "" {
		return mutedStyle.Render(strings.Repeat("░", width))
	}
	filled := accentStyle.Render(strings.Repeat("█", full))
	if lead && full > 0 {
		filled = accentStyle.Render(strings.Repeat("█", full-1)) + valueStyle.Render("█")
	}
	rest := width - full
	if part != "" {
		filled += accentStyle.Render(part)
		rest--
	}
	return filled + mutedStyle.Render(strings.Repeat("░", rest))
}

func rateLine(name string, rate, scale float64, w int) string {
	bar := renderBar(rate, scale, max(12, w-34), true)
	return fmt.Sprintf("%s %s %12s", labelStyle.Render(name), bar, valueStyle.Render(fmtRate(rate)))
}

// latencyLine holds the last round trip on screen across lost pings. A streak
// annotates the value beside it, and three in a row reads as a timeout.
func latencyLine(s goclient.LatencySample, lostStreak int) string {
	if s.RTT <= 0 {
		if lostStreak > 0 {
			return labelStyle.Render("latency ") + errorStyle.Render("timeout")
		}
		return labelStyle.Render("latency ") + mutedStyle.Render("waiting")
	}
	load := ""
	if s.UnderLoad {
		load = " loaded"
	}
	line := labelStyle.Render("latency ") + valueStyle.Render(fmtMs(s.RTT)) + mutedStyle.Render(load)
	switch {
	case lostStreak >= 3:
		line += errorStyle.Render("  timeout ×" + fmt.Sprint(lostStreak))
	case lostStreak > 0:
		line += warnStyle.Render(fmt.Sprintf("  %d lost", lostStreak))
	}
	return line
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

// fmtClock renders a running clock: tenths under a minute, whole seconds
// above, where tenths only flicker.
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

func clamp(v, lo, hi int) int {
	if hi < lo {
		return lo
	}
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}
