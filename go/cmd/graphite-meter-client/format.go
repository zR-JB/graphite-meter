package main

import (
	"cmp"
	"fmt"
	"net/url"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func protocolChoiceLabel(protocol string) string {
	if protocol == "auto" {
		return "Automatic"
	}
	return goclient.ProtocolLabel(protocol)
}

func stageSummary(s goclient.StageSet) string {
	var parts []string
	for _, stage := range (goclient.Config{Stages: s}).Plan() {
		parts = append(parts, stage.Name)
	}
	if len(parts) == 0 {
		return "none"
	}
	return strings.Join(parts, ", ")
}

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

const (
	labelColumn = 18
	fieldColumn = 11
)

func field(label, value string) string {
	return labelStyle.Render(pad(label, fieldColumn)) + value
}

func toggleLine(label string, on bool, note string) string {
	return fmt.Sprintf("%s %-*s %s", checkbox(on), labelColumn-2, label, mutedStyle.Render(note))
}

func valueLine(label, value, note string) string {
	return fmt.Sprintf("%-*s %s  %s", labelColumn, label, valueStyle.Render(value), mutedStyle.Render(note))
}

func inertValueLine(label, value, note string) string {
	return fmt.Sprintf("%s %s  %s", mutedStyle.Render(pad(label, labelColumn)), mutedStyle.Render(value), mutedStyle.Render(note))
}

func pathRow(label, target, transport string, choices []pathChoice) string {
	value, note, pos := unofferedPathLabel(target, transport), "", ""
	for i, c := range choices {
		if c.selects(target, transport) {
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

func unofferedPathLabel(target, transport string) string {
	if target == "auto" && transport == "auto" {
		return "Automatic"
	}
	mechanism := map[string]string{
		"auto":                             "Automatic transport",
		wire.TransportFetchStream:          "Fetch stream",
		wire.TransportWebSocket:            "WebSocket",
		wire.TransportWebTransport:         "WebTransport",
		wire.TransportWebTransportDatagram: "WebTransport datagrams",
	}[transport]
	mechanism = cmp.Or(mechanism, emptyDash(transport))
	if target == "auto" {
		return mechanism + " · automatic origin"
	}
	return mechanism + " · " + target
}

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

// eighths are the partial-cell fills between an empty and a full block. A bar's tip moves in sub-cell steps.
var eighths = []string{"", "▏", "▎", "▍", "▌", "▋", "▊", "▉"}

func renderBar(value, scale float64, width int, lead bool) string {
	cells := 0.0
	if scale > 0 {
		cells = value / scale * float64(width)
		cells = min(max(cells, 0), float64(width))
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

func latencyLine(s goclient.LatencySample, lostStreak int) string {
	if s.RTT <= 0 {
		if lostStreak > 0 {
			return labelStyle.Render("latency ") + errorStyle.Render("probe timeout")
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
		line += errorStyle.Render("  probe timeout ×" + fmt.Sprint(lostStreak))
	case lostStreak > 0:
		line += warnStyle.Render(fmt.Sprintf("  probe timeout ×%d", lostStreak))
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

// fmtClock renders a running clock: tenths under a minute, whole seconds above, where tenths only flicker.
func fmtClock(d time.Duration) string {
	d = max(d, 0)
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

func latencyOutcomeSummary(s goclient.LatencyStats) string {
	variation := "--"
	if s.JitterPairs > 0 {
		variation = fmt.Sprintf("%.2f ms", float64(s.Jitter)/float64(time.Millisecond))
	}
	timeouts := "-- (no resolved probes)"
	if ratio, ok := s.TimeoutRatio(); ok {
		timeouts = fmt.Sprintf("%.1f%% (%d/%d)", ratio*100, s.Timeouts, s.Count+s.Timeouts)
	}
	out := "RTT variation " + variation + "  probe timeouts " + timeouts
	if s.Unresolved > 0 {
		out += fmt.Sprintf("  unresolved %d", s.Unresolved)
	}
	if s.SendFailures > 0 {
		out += fmt.Sprintf("  send failures %d", s.SendFailures)
	}
	return out
}
