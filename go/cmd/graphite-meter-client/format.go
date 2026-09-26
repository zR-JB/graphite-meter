package main

import (
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// Units and precision follow the browser's format.ts; "—" marks missing data.
const missing = "—"

var rateUnits = []string{"bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"}

// fmtRate promotes the unit only past 1.2× the next tier.
func fmtRate(bytesPerSec float64) string {
	bits := bytesPerSec * 8
	tier := 0
	for tier < len(rateUnits)-1 && bits >= 1.2*math.Pow(1000, float64(tier+1)) {
		tier++
	}
	return fmtSpeed(bits/math.Pow(1000, float64(tier))) + " " + rateUnits[tier]
}

func fmtSpeed(value float64) string {
	switch {
	case value >= 1000:
		return strconv.FormatFloat(value, 'f', 0, 64)
	case value >= 100:
		return strconv.FormatFloat(value, 'f', 1, 64)
	}
	return strconv.FormatFloat(value, 'f', 2, 64)
}

func fmtBytes(n uint64) string {
	units := []string{"B", "kB", "MB", "GB", "TB"}
	value, tier := float64(n), 0
	for value >= 1000 && tier < len(units)-1 {
		value /= 1000
		tier++
	}
	if tier == 0 {
		return fmt.Sprintf("%d B", n)
	}
	return fmt.Sprintf("%.1f %s", value, units[tier])
}

func fmtMs(d time.Duration) string {
	ms := float64(d) / float64(time.Millisecond)
	if math.Abs(ms) < 100 {
		return fmt.Sprintf("%.1f ms", ms)
	}
	return fmt.Sprintf("%.0f ms", ms)
}

// fmtAdded keeps the sign.
func fmtAdded(d time.Duration) string {
	if d < 0 {
		return "−" + fmtMs(-d)
	}
	return "+" + fmtMs(d)
}

// fmtSetting renders a configured duration: 800 ms, 4 s, 1.5 s.
func fmtSetting(d time.Duration) string {
	if d < time.Second {
		return fmt.Sprintf("%d ms", d.Milliseconds())
	}
	return strconv.FormatFloat(d.Seconds(), 'f', -1, 64) + " s"
}

// fmtClock renders a running or measured span in tenths of a second.
func fmtClock(d time.Duration) string {
	return fmt.Sprintf("%.1f s", max(d, 0).Seconds())
}

var stageLabels = map[goclient.Stage]string{
	goclient.StageLatency:       "Latency",
	goclient.StageDownload:      "Download",
	goclient.StageUpload:        "Upload",
	goclient.StageBidirectional: "Bidirectional",
}

// compactStage is the stage name where a column is narrow.
func compactStage(stage goclient.Stage) string {
	if stage == goclient.StageBidirectional {
		return "Bi-dir"
	}
	return stageLabels[stage]
}

// populationLabel names a latency population by the load it was measured under.
func populationLabel(stage goclient.Stage) string {
	if stage == goclient.StageLatency {
		return "Idle latency"
	}
	return "Loaded latency · " + compactStage(stage)
}

func directionLabel(r goclient.Result) string {
	if r.Stage != goclient.StageBidirectional {
		return stageLabels[r.Stage]
	}
	if r.Direction == goclient.Up {
		return "Bi-dir ↑"
	}
	return "Bi-dir ↓"
}

// latencyParts reports a population, median first.
func latencyParts(s goclient.LatencyStats, idle *goclient.LatencyStats) []string {
	median := missing
	if s.Count > 0 {
		median = fmtMs(s.P50)
	}
	parts := []string{"median " + median}
	if idle != nil && s.Count > 0 && idle.Count > 0 {
		parts = append(parts, fmtAdded(s.P50-idle.P50)+" added")
	}
	p95, jitter := missing, missing
	if s.Count > 0 {
		p95 = fmtMs(s.P95)
	}
	if s.JitterPairs > 0 {
		jitter = fmtMs(s.Jitter)
	}
	timeouts := missing
	if ratio, ok := s.TimeoutRatio(); ok {
		timeouts = fmt.Sprintf("%d/%d (%.1f%%)", s.Timeouts, s.Count+s.Timeouts, ratio*100)
	}
	parts = append(parts, "p95 "+p95, "jitter "+jitter, "probe timeouts "+timeouts, fmt.Sprintf("%d replies", s.Count))
	if s.Elapsed > 0 {
		parts = append(parts, fmtClock(s.Elapsed))
	}
	if s.Unresolved > 0 {
		parts = append(parts, fmt.Sprintf("unfinished probes %d", s.Unresolved))
	}
	if s.SendFailures > 0 {
		parts = append(parts, fmt.Sprintf("failed sends %d", s.SendFailures))
	}
	return parts
}

// wrapParts joins facts with " · ", breaking only between facts.
func wrapParts(parts []string, w int) []string {
	var lines []string
	line := ""
	for _, part := range parts {
		switch {
		case line == "":
			line = part
		case len([]rune(line))+3+len([]rune(part)) <= w:
			line += " · " + part
		default:
			lines = append(lines, line)
			line = part
		}
	}
	return append(lines, line)
}

func reflectorTimingSummary(s *goclient.ReflectorTimingStats) string {
	if s == nil {
		return ""
	}
	return fmt.Sprintf("Server timing (%d paired replies, means): raw %s · handling %s · adjusted %s. Only server handling is subtracted.",
		s.Count, fmtMs(s.MeanRawRTT), fmtMs(s.MeanHandling), fmtMs(s.MeanAdjustedRTT))
}

func protocolChoiceLabel(protocol string) string {
	if protocol == "auto" {
		return "Automatic"
	}
	return goclient.ProtocolLabel(protocol)
}

// eighths are partial-cell fills, so a bar grows in sub-cell steps.
var eighths = []string{"", "▏", "▎", "▍", "▌", "▋", "▊", "▉"}

func renderBar(value, scale float64, width int) string {
	cells := 0.0
	if scale > 0 {
		cells = min(max(value/scale*float64(width), 0), float64(width))
	}
	full := int(cells)
	part := eighths[int((cells-float64(full))*8)]
	rest := width - full
	if part != "" {
		rest--
	}
	return accentStyle.Render(strings.Repeat("█", full)+part) + mutedStyle.Render(strings.Repeat("░", rest))
}

func pad(s string, w int) string {
	return s + strings.Repeat(" ", max(0, w-len([]rune(s))))
}

func checkbox(on bool) string {
	if on {
		return accentStyle.Render("●")
	}
	return mutedStyle.Render("○")
}
